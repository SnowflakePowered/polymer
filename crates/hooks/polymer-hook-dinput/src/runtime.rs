//! DirectInput device interception and forwarded input state.
#![allow(unsafe_code)]
#![allow(non_snake_case)]

use crate::vtable::{DeviceHookState, IDirectInputDeviceHook, IDirectInputHook};
use polymer_hook::config;
use polymer_hook_input_dinput::Runtime as InputRuntime;
use polymer_protocol::input::DirectInputDevice;
use std::{
    ffi::c_void,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use windows::{
    Win32::Devices::HumanInterfaceDevice::{
        GUID_SysKeyboard, GUID_SysMouse, IDirectInput8A, IDirectInput8W,
    },
    core::{GUID, Interface},
};
use windows_sys::{
    Win32::{
        Devices::HumanInterfaceDevice::{DIDEVICEOBJECTDATA, DIGDD_PEEK},
        Foundation::HINSTANCE,
    },
    core::{GUID as SysGuid, HRESULT},
};

pub(crate) struct Runtime {
    logged_delivery: AtomicBool,
    logged_keyboard_poll: AtomicBool,
    logged_keyboard_pressed: AtomicBool,
    logged_mouse_poll: AtomicBool,
    logged_mouse_activity: AtomicBool,
    input: Arc<InputRuntime>,
}

impl Default for Runtime {
    fn default() -> Self {
        polymer_hook::telemetry::init_file_tracing(config::log_file(config::DINPUT_LOG_FILE));
        Self {
            logged_delivery: AtomicBool::new(false),
            logged_keyboard_poll: AtomicBool::new(false),
            logged_keyboard_pressed: AtomicBool::new(false),
            logged_mouse_poll: AtomicBool::new(false),
            logged_mouse_activity: AtomicBool::new(false),
            input: Arc::new(InputRuntime::new()),
        }
    }
}

impl Runtime {
    pub(crate) fn start_input_receiver(self: &Arc<Self>) {
        let Ok(session) = config::session() else {
            return;
        };
        self.input.start_receiver(format!("{session}-input"));
    }

    pub(crate) fn set_event_notification(
        &self,
        key: usize,
        device: DirectInputDevice,
        event: usize,
    ) {
        self.input.set_event_notification(key, device, event);
    }

    unsafe fn wrap_direct_input(
        self: &Arc<Self>,
        interface: *mut c_void,
        iid: &GUID,
    ) -> Option<*mut c_void> {
        let wide = match *iid {
            IDirectInput8W::IID => true,
            IDirectInput8A::IID => false,
            _ => return None,
        };
        unsafe {
            IDirectInputHook::wrap::<IDirectInput8A>(
                interface,
                *iid,
                crate::vtable::DirectInputContext {
                    runtime: self.clone(),
                    wide,
                },
            )
        }
    }
}

pub(crate) unsafe fn create_device(
    class: *const GUID,
    output: *mut *mut c_void,
    result: HRESULT,
    context: &crate::vtable::CreateDeviceContext<'_>,
) {
    let runtime = context.state.runtime.clone();
    if !class.is_null() {
        let class = unsafe { &*class };
        tracing::debug!(
            "polymer: DirectInput CreateDevice {:08x}-{:04x}-{:04x}",
            class.data1,
            class.data2,
            class.data3
        );
    }
    tracing::debug!(
        "polymer: DirectInput CreateDevice returned {result:#x} interface={:#x}",
        if output.is_null() {
            0
        } else {
            (unsafe { *output }) as usize
        }
    );
    if result >= 0 && !class.is_null() && !output.is_null() {
        let class = unsafe { &*class };
        let device_class = if class == &GUID_SysKeyboard {
            Some(DirectInputDevice::Keyboard)
        } else if class == &GUID_SysMouse {
            Some(DirectInputDevice::Mouse)
        } else {
            None
        };
        if let Some(device_class) = device_class {
            if let Some(proxy) =
                unsafe { wrap_device(runtime, *output, device_class, context.state.wide) }
            {
                unsafe { *output = proxy };
            }
            tracing::debug!("polymer: hooked {device_class:?} DirectInput device");
        }
    }
}

unsafe fn wrap_device(
    runtime: Arc<Runtime>,
    interface: *mut c_void,
    device: DirectInputDevice,
    wide: bool,
) -> Option<*mut c_void> {
    let iid = if wide {
        windows::Win32::Devices::HumanInterfaceDevice::IDirectInputDevice8W::IID
    } else {
        windows::Win32::Devices::HumanInterfaceDevice::IDirectInputDevice8A::IID
    };
    unsafe {
        IDirectInputDeviceHook::wrap::<
            windows::Win32::Devices::HumanInterfaceDevice::IDirectInputDevice8A,
        >(
            interface,
            iid,
            crate::vtable::DeviceContext { runtime, device },
        )
    }
}

pub(crate) unsafe fn acquire(
    this: *mut c_void,
    mut next: crate::vtable::AcquireContext<'_>,
) -> HRESULT {
    if next.state.runtime.input.connected() {
        // The target lives on a non-interactive desktop in isolate mode, so the
        // system device cannot become foreground-acquired. Input is supplied by
        // Polymer in that case and does not depend on the system acquisition.
        return 0;
    }
    polymer_hook::next!(next, this)
}

pub(crate) unsafe fn set_cooperative_level(
    this: *mut c_void,
    window: *mut c_void,
    flags: u32,
    mut next: crate::vtable::SetCooperativeLevelContext<'_>,
) -> HRESULT {
    if next.state.runtime.input.connected() {
        // A cooperative level against a window on the hidden desktop can be
        // rejected even though this proxy owns the complete device state.
        return 0;
    }
    polymer_hook::next!(next, this, window, flags)
}

pub(crate) unsafe fn get_device_state(
    this: *mut c_void,
    size: u32,
    output: *mut c_void,
    mut next: crate::vtable::GetDeviceStateContext<'_>,
) -> HRESULT {
    let device = &next.state.device;
    if !next.state.runtime.input.connected() {
        return polymer_hook::next!(next, this, size, output);
    }
    if output.is_null() {
        return 0x8007_0057u32 as i32;
    }
    match device {
        DirectInputDevice::Keyboard => {
            let state = next.state.runtime.input.keyboard_state();
            if size as usize != state.len() {
                return 0x8007_0057u32 as i32;
            }
            if !next
                .state
                .runtime
                .logged_keyboard_poll
                .swap(true, Ordering::AcqRel)
            {
                tracing::debug!("polymer: target first polled forwarded keyboard state");
            }
            if state.iter().any(|state| *state != 0)
                && !next
                    .state
                    .runtime
                    .logged_keyboard_pressed
                    .swap(true, Ordering::AcqRel)
            {
                tracing::debug!("polymer: target polled a pressed forwarded keyboard key");
            }
            unsafe {
                std::ptr::copy_nonoverlapping(state.as_ptr(), output.cast::<u8>(), state.len());
            }
        }
        DirectInputDevice::Mouse => {
            // DIMOUSESTATE is 16 bytes (4 buttons); DIMOUSESTATE2 is 20 bytes
            // (8 buttons). Both begin with three signed relative axes.
            if size != 16 && size != 20 {
                return 0x8007_0057u32 as i32;
            }
            if !next
                .state
                .runtime
                .logged_mouse_poll
                .swap(true, Ordering::AcqRel)
            {
                tracing::debug!("polymer: target first polled forwarded mouse state");
            }
            let state = next.state.runtime.input.take_mouse_state();
            if (state.axes != [0; 3] || state.buttons != [0; 8])
                && !next
                    .state
                    .runtime
                    .logged_mouse_activity
                    .swap(true, Ordering::AcqRel)
            {
                tracing::debug!(
                    "polymer: target polled forwarded mouse activity axes={:?}",
                    state.axes
                );
            }
            let output = output.cast::<u8>();
            for (index, axis) in state.axes.iter().enumerate() {
                unsafe { std::ptr::write_unaligned(output.add(index * 4).cast::<i32>(), *axis) };
            }
            let button_count = size as usize - 12;
            unsafe {
                std::ptr::copy_nonoverlapping(state.buttons.as_ptr(), output.add(12), button_count);
            }
        }
    }
    0
}

pub(crate) unsafe fn device_destroyed(this: *mut c_void, state: &dyn DeviceHookState) {
    state.state().runtime.input.remove_device(this as usize);
}

pub(crate) unsafe fn get_device_data(
    this: *mut c_void,
    object_size: u32,
    output: *mut DIDEVICEOBJECTDATA,
    count: *mut u32,
    flags: u32,
    mut next: crate::vtable::GetDeviceDataContext<'_>,
) -> HRESULT {
    let device = &next.state.device;
    if !next.state.runtime.input.connected() {
        return polymer_hook::next!(next, this, object_size, output, count, flags);
    }
    if count.is_null() {
        return 0x8007_0057u32 as i32;
    }

    let requested = if output.is_null() {
        next.state.runtime.input.buffered_len(*device)
    } else {
        (unsafe { *count }) as usize
    };
    if output.is_null() {
        unsafe { *count = requested as u32 };
        return 0;
    }
    if object_size < 16 {
        return 0x8007_0057u32 as i32;
    }
    let peek = flags & DIGDD_PEEK != 0;
    let events = next
        .state
        .runtime
        .input
        .buffered_data(*device, requested, peek);
    unsafe { *count = events.len() as u32 };
    for (index, event) in events.iter().copied().enumerate() {
        let destination = unsafe { (output.cast::<u8>()).add(index * object_size as usize) };
        unsafe {
            std::ptr::write_unaligned(destination.cast::<u32>(), event.offset);
            std::ptr::write_unaligned(destination.add(4).cast::<u32>(), event.data);
            std::ptr::write_unaligned(destination.add(8).cast::<u32>(), event.timestamp);
            std::ptr::write_unaligned(destination.add(12).cast::<u32>(), event.sequence);
            if object_size as usize >= std::mem::size_of::<DIDEVICEOBJECTDATA>() {
                std::ptr::write_unaligned(destination.add(16).cast::<usize>(), 0);
            }
        }
    }
    if !events.is_empty()
        && !next
            .state
            .runtime
            .logged_delivery
            .swap(true, Ordering::AcqRel)
    {
        tracing::debug!("polymer: returned first forwarded {device:?} record from GetDeviceData");
    }
    0
}

pub(crate) unsafe fn direct_input_8_create(
    instance: HINSTANCE,
    version: u32,
    interface_id: *const SysGuid,
    output: *mut *mut c_void,
    outer: *mut c_void,
    result: HRESULT,
    context: &crate::hooks::DirectInput8CreateContext,
) {
    context.state.start_input_receiver();
    tracing::debug!(
        "polymer: DirectInput8Create version={version:#x} iid={interface_id:p} returned {result:#x}"
    );
    if result >= 0 && !interface_id.is_null() && !output.is_null() && outer.is_null() {
        let interface_id = unsafe { &*interface_id.cast::<GUID>() };
        if let Some(proxy) = unsafe { context.state.wrap_direct_input(*output, interface_id) } {
            unsafe { *output = proxy };
        }
    }
    let _ = (instance, outer);
}
