use dashmap::DashMap;
use polymer_platform_windows::NativeWindow;
use std::{
    mem::{offset_of, size_of},
    sync::atomic::{AtomicBool, AtomicIsize, Ordering},
};
use windows_sys::Win32::{
    Foundation::{ERROR_INSUFFICIENT_BUFFER, HANDLE, SetLastError},
    UI::{
        Input::{
            HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RAWKEYBOARD, RID_HEADER,
            RID_INPUT, RIDEV_REMOVE, RIM_TYPEKEYBOARD,
        },
        WindowsAndMessaging::{
            PostMessageW, RI_KEY_BREAK, RI_KEY_E0, RIM_INPUT, WM_INPUT, WM_KEYDOWN, WM_KEYUP,
        },
    },
};

const HANDLE_TAG: isize = 0x504B_0000_0000;
const HANDLE_MASK: isize = !0xFFFF_FFFF;
const RETAINED: isize = 256;
const KEYBOARD_SIZE: u32 = (offset_of!(RAWINPUT, data) + size_of::<RAWKEYBOARD>()) as u32;
const HEADER_SIZE: u32 = size_of::<RAWINPUTHEADER>() as u32;

#[derive(Default)]
pub struct RawKeyboard {
    registered: AtomicBool,
    sequence: AtomicIsize,
    pending: DashMap<isize, RAWKEYBOARD>,
}

impl RawKeyboard {
    pub fn register(&self, devices: &[RAWINPUTDEVICE]) {
        for device in devices
            .iter()
            .filter(|device| device.usUsagePage == 1 && device.usUsage == 6)
        {
            self.registered
                .store(device.dwFlags & RIDEV_REMOVE == 0, Ordering::Release);
        }
    }

    pub fn post(
        &self,
        window: NativeWindow,
        virtual_key: u32,
        scan_code: u16,
        extended: bool,
        pressed: bool,
    ) {
        if !self.registered.load(Ordering::Acquire) {
            return;
        }
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) & 0xFFFF_FFFF;
        self.pending
            .remove(&(HANDLE_TAG | ((sequence - RETAINED) & 0xFFFF_FFFF)));
        let handle = HANDLE_TAG | sequence;
        self.pending.insert(
            handle,
            RAWKEYBOARD {
                MakeCode: scan_code,
                Flags: (if pressed { 0 } else { RI_KEY_BREAK }
                    | if extended { RI_KEY_E0 } else { 0 }) as u16,
                Reserved: 0,
                VKey: virtual_key as u16,
                Message: if pressed { WM_KEYDOWN } else { WM_KEYUP },
                ExtraInformation: 0,
            },
        );
        unsafe { PostMessageW(window.0 as _, WM_INPUT, RIM_INPUT as usize, handle) };
    }

    /// # Safety
    ///
    /// `data` and `size` must satisfy `GetRawInputData`'s contract.
    pub unsafe fn data(
        &self,
        handle: HRAWINPUT,
        command: u32,
        data: *mut core::ffi::c_void,
        size: *mut u32,
        header_size: u32,
    ) -> Option<u32> {
        let handle = handle as isize;
        if handle & HANDLE_MASK != HANDLE_TAG {
            return None;
        }
        let keyboard = *self.pending.get(&handle)?;
        let required = match command {
            RID_INPUT => KEYBOARD_SIZE,
            RID_HEADER => HEADER_SIZE,
            _ => return Some(u32::MAX),
        };
        if header_size != HEADER_SIZE || size.is_null() {
            return Some(u32::MAX);
        }
        if data.is_null() {
            unsafe { *size = required };
            return Some(0);
        }
        if unsafe { *size } < required {
            unsafe {
                *size = required;
                SetLastError(ERROR_INSUFFICIENT_BUFFER);
            }
            return Some(u32::MAX);
        }
        let header = RAWINPUTHEADER {
            dwType: RIM_TYPEKEYBOARD,
            dwSize: KEYBOARD_SIZE,
            hDevice: core::ptr::null_mut::<core::ffi::c_void>() as HANDLE,
            wParam: RIM_INPUT as usize,
        };
        let input = data.cast::<RAWINPUT>();
        unsafe {
            core::ptr::addr_of_mut!((*input).header).write_unaligned(header);
            if command == RID_INPUT {
                core::ptr::addr_of_mut!((*input).data.keyboard).write_unaligned(keyboard);
            }
        }
        Some(required)
    }
}
