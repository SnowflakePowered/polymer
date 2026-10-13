use winit::raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawDisplayHandle,
    RawWindowHandle, WindowHandle,
};
use winit::window::Window;

/// The shell window as the owner of a system dialog opened from another thread.
#[derive(Clone, Copy, Debug)]
pub struct DialogParent {
    window: RawWindowHandle,
    display: RawDisplayHandle,
}

// SAFETY: the handles are identifiers. Dialog backends use them on the threads
// the platform requires, and the shell window outlives every dialog it owns.
unsafe impl Send for DialogParent {}

impl DialogParent {
    pub fn of(window: &Window) -> Option<Self> {
        Some(Self {
            window: window.window_handle().ok()?.as_raw(),
            display: window.display_handle().ok()?.as_raw(),
        })
    }
}

impl HasWindowHandle for DialogParent {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        // SAFETY: see the `Send` implementation.
        Ok(unsafe { WindowHandle::borrow_raw(self.window) })
    }
}

impl HasDisplayHandle for DialogParent {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        // SAFETY: see the `Send` implementation.
        Ok(unsafe { DisplayHandle::borrow_raw(self.display) })
    }
}
