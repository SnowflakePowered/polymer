use blitz_traits::shell::{ClipboardError, ShellProvider};
use std::sync::Mutex;

pub(super) struct NativeShellProvider {
    proxy: crate::window_manager::EventLoopProxy<crate::app::UserEvent>,
    clipboard: Mutex<Option<arboard::Clipboard>>,
}

impl NativeShellProvider {
    pub(super) fn new(proxy: crate::window_manager::EventLoopProxy<crate::app::UserEvent>) -> Self {
        Self {
            proxy,
            clipboard: Mutex::new(arboard::Clipboard::new().ok()),
        }
    }

    fn send(&self, command: crate::window_manager::WindowCommand) {
        let _ = self
            .proxy
            .send_event(crate::commands::UserEvent::Window(command));
    }
}

impl ShellProvider for NativeShellProvider {
    fn request_redraw(&self) {
        self.send(crate::window_manager::WindowCommand::RedrawShell);
    }

    fn set_cursor(&self, icon: Option<cursor_icon::CursorIcon>) {
        self.send(crate::window_manager::WindowCommand::SetCursor(icon));
    }

    fn set_window_title(&self, title: String) {
        self.send(crate::window_manager::WindowCommand::SetTitle(title));
    }

    fn set_ime_enabled(&self, is_enabled: bool) {
        self.send(crate::window_manager::WindowCommand::SetImeEnabled(
            is_enabled,
        ));
    }

    fn set_ime_cursor_area(&self, x: f32, y: f32, width: f32, height: f32) {
        self.send(crate::window_manager::WindowCommand::SetImeCursorArea {
            x,
            y,
            width,
            height,
        });
    }

    fn get_clipboard_text(&self) -> Result<String, ClipboardError> {
        self.clipboard
            .lock()
            .map_err(|_| ClipboardError)?
            .as_mut()
            .ok_or(ClipboardError)?
            .get_text()
            .map_err(|_| ClipboardError)
    }

    fn set_clipboard_text(&self, text: String) -> Result<(), ClipboardError> {
        self.clipboard
            .lock()
            .map_err(|_| ClipboardError)?
            .as_mut()
            .ok_or(ClipboardError)?
            .set_text(text)
            .map_err(|_| ClipboardError)
    }
}
