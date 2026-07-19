use super::pointer::{DisplayedSurface, PointerRoutingState, PointerTarget};
use snowglass_input::{DirectInputData, DirectInputDevice, mouse::layout::Extent};
use snowglass_ipc::{LocalSocketListener, Message};
use snowglass_platform_windows::{NativeWindow, forward_raw_key};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicIsize, AtomicU32, Ordering},
        mpsc,
    },
    thread,
    time::Instant,
};

struct RawKeyboardContext {
    sender: mpsc::Sender<Message>,
    backing_window: Arc<AtomicIsize>,
    sequence: AtomicU32,
    started: Instant,
    pressed: [bool; 256],
}

pub(crate) struct RawKeyboardForwarder {
    window: isize,
    context: *mut RawKeyboardContext,
}

impl RawKeyboardForwarder {
    pub(crate) fn install(
        window: NativeWindow,
        sender: mpsc::Sender<Message>,
        backing_window: Arc<AtomicIsize>,
    ) -> std::io::Result<Self> {
        use windows_sys::Win32::UI::{
            Input::{RAWINPUTDEVICE, RIDEV_INPUTSINK, RegisterRawInputDevices},
            Shell::SetWindowSubclass,
        };
        let context = Box::into_raw(Box::new(RawKeyboardContext {
            sender,
            backing_window,
            sequence: AtomicU32::new(0),
            started: Instant::now(),
            pressed: [false; 256],
        }));
        let device = RAWINPUTDEVICE {
            usUsagePage: 1,
            usUsage: 6,
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: window.0 as _,
        };
        if unsafe {
            RegisterRawInputDevices(&device, 1, std::mem::size_of::<RAWINPUTDEVICE>() as u32)
        } == 0
            || unsafe {
                SetWindowSubclass(
                    window.0 as _,
                    Some(raw_keyboard_subclass),
                    context as usize,
                    context as usize,
                )
            } == 0
        {
            unsafe { drop(Box::from_raw(context)) };
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            window: window.0,
            context,
        })
    }
}

impl Drop for RawKeyboardForwarder {
    fn drop(&mut self) {
        use windows_sys::Win32::UI::Shell::RemoveWindowSubclass;
        unsafe {
            RemoveWindowSubclass(
                self.window as _,
                Some(raw_keyboard_subclass),
                self.context as usize,
            );
            drop(Box::from_raw(self.context));
        }
    }
}

unsafe extern "system" fn raw_keyboard_subclass(
    window: windows_sys::Win32::Foundation::HWND,
    message: u32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
    _id: usize,
    context: usize,
) -> windows_sys::Win32::Foundation::LRESULT {
    use windows_sys::Win32::UI::{
        Input::{GetRawInputData, RAWINPUT, RID_INPUT, RIM_TYPEKEYBOARD},
        Shell::DefSubclassProc,
        WindowsAndMessaging::{RI_KEY_BREAK, RI_KEY_E0, RI_KEY_E1, WM_INPUT},
    };
    if message == WM_INPUT {
        let mut input = RAWINPUT::default();
        let mut size = std::mem::size_of::<RAWINPUT>() as u32;
        if unsafe {
            GetRawInputData(
                lparam as _,
                RID_INPUT,
                (&mut input as *mut RAWINPUT).cast(),
                &mut size,
                std::mem::size_of_val(&input.header) as u32,
            )
        } != u32::MAX
            && input.header.dwType == RIM_TYPEKEYBOARD
        {
            let keyboard = unsafe { input.data.keyboard };
            if keyboard.MakeCode != 0 {
                let extended = keyboard.Flags & (RI_KEY_E0 | RI_KEY_E1) as u16 != 0;
                let offset = u32::from(keyboard.MakeCode) | u32::from(extended) * 0x80;
                let context = unsafe { &mut *(context as *mut RawKeyboardContext) };
                let pressed = keyboard.Flags & RI_KEY_BREAK as u16 == 0;
                let repeat = pressed && context.pressed[offset as usize];
                context.pressed[offset as usize] = pressed;
                let sequence = context.sequence.fetch_add(1, Ordering::Relaxed) + 1;
                let _ = context
                    .sender
                    .send(Message::DirectInputData(DirectInputData {
                        device: DirectInputDevice::Keyboard,
                        offset,
                        data: if pressed { 0x80 } else { 0 },
                        timestamp: context.started.elapsed().as_millis() as u32,
                        sequence,
                    }));
                let backing = context.backing_window.load(Ordering::Acquire);
                if backing != 0 {
                    forward_raw_key(
                        NativeWindow(backing),
                        u32::from(keyboard.VKey),
                        keyboard.MakeCode,
                        extended,
                        pressed,
                        repeat,
                    );
                }
            }
        }
    }
    unsafe { DefSubclassProc(window, message, wparam, lparam) }
}

pub(crate) fn spawn_input_ipc(listener: LocalSocketListener) -> mpsc::Sender<Message> {
    let (sender, input) = mpsc::channel();
    thread::spawn(move || {
        let mut pending = None;
        loop {
            let mut socket = match listener.accept() {
                Ok(socket) => {
                    eprintln!("snowglass: target input IPC connected");
                    socket
                }
                Err(error) => {
                    eprintln!("snowglass: DirectInput IPC accept ended: {error}");
                    return;
                }
            };
            loop {
                let event = match pending.take() {
                    Some(event) => event,
                    None => match input.recv() {
                        Ok(event) => event,
                        Err(_) => return,
                    },
                };
                if let Err(error) = socket.send(event) {
                    pending = Some(event);
                    if error.kind() != std::io::ErrorKind::BrokenPipe
                        && error.kind() != std::io::ErrorKind::ConnectionReset
                    {
                        eprintln!("snowglass: DirectInput IPC send failed: {error}");
                    }
                    break;
                }
            }
        }
    });
    sender
}

pub(crate) struct DirectInputForwarder {
    sender: mpsc::Sender<Message>,
    sequence: u32,
    started: Instant,
}

impl DirectInputForwarder {
    pub(crate) fn new(sender: mpsc::Sender<Message>) -> Self {
        Self {
            sender,
            sequence: 0,
            started: Instant::now(),
        }
    }

    pub(crate) fn mouse_button(&mut self, button: u16, pressed: bool) {
        self.send(
            DirectInputDevice::Mouse,
            12 + u32::from(button.saturating_sub(1)),
            if pressed { 0x80 } else { 0 },
        );
    }

    pub(crate) fn mouse_wheel(&mut self, vertical: i32) {
        if vertical != 0 {
            self.send(DirectInputDevice::Mouse, 8, vertical as u32);
        }
    }

    pub(crate) fn mouse_motion(&mut self, delta: (f64, f64)) {
        for (offset, value) in [(0, delta.0), (4, delta.1)] {
            if value != 0.0 {
                self.send(DirectInputDevice::Mouse, offset, (value as i32) as u32);
            }
        }
    }

    fn release_mouse_buttons(&mut self, buttons: u32) {
        for index in 0..32 {
            if buttons & (1 << index) != 0 {
                self.send(DirectInputDevice::Mouse, 12 + index, 0);
            }
        }
    }

    fn send(&mut self, device: DirectInputDevice, offset: u32, data: u32) {
        self.sequence = self.sequence.wrapping_add(1);
        let _ = self.sender.send(Message::DirectInputData(DirectInputData {
            device,
            offset,
            data,
            timestamp: self.started.elapsed().as_millis() as u32,
            sequence: self.sequence,
        }));
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn cancel_pointer_connection(
    connection: u64,
    routing: &mut PointerRoutingState,
    viewer_extent: Extent,
    surfaces: &[DisplayedSurface],
    targets: &HashMap<u64, PointerTarget>,
    sender: &mpsc::Sender<Message>,
    direct_input: &mut DirectInputForwarder,
) {
    let Some(buttons) = routing.cancel(connection, viewer_extent, surfaces, targets, sender) else {
        return;
    };
    direct_input.release_mouse_buttons(buttons);
}
