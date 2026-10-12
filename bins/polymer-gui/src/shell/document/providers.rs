use super::urls::ShellUrls;
use blitz_traits::net::{Bytes, NetHandler, NetProvider, NetWaker, Request};
use blitz_traits::shell::{ClipboardError, ShellProvider};
use parking_lot::Mutex;
use std::sync::Arc;

pub(super) struct ShellNetProvider {
    remote: Arc<dyn NetProvider>,
    waker: Arc<dyn NetWaker>,
    urls: Arc<ShellUrls>,
}

impl ShellNetProvider {
    pub(super) fn new(waker: Arc<dyn NetWaker>, urls: Arc<ShellUrls>) -> Self {
        Self {
            remote: blitz_net::Provider::shared(Some(waker.clone())),
            waker,
            urls,
        }
    }
}

struct CompletedAssetHandler {
    handler: Box<dyn NetHandler>,
    requested: String,
    waker: Arc<dyn NetWaker>,
    doc_id: usize,
}

impl NetHandler for CompletedAssetHandler {
    fn bytes(self: Box<Self>, _resolved_url: String, bytes: Bytes) {
        self.handler.bytes(self.requested, bytes);
        // blitz-net wakes before decoding; wake again once the image is available.
        self.waker.wake(self.doc_id);
    }
}

impl NetProvider for ShellNetProvider {
    fn fetch(&self, doc_id: usize, mut request: Request, handler: Box<dyn NetHandler>) {
        let Some(url) = self.urls.resolve(&request.url) else {
            tracing::warn!(url = %request.url, "polymer: unresolvable shell asset");
            handler.bytes(request.url.to_string(), Bytes::new());
            self.waker.wake(doc_id);
            return;
        };
        let requested = std::mem::replace(&mut request.url, url).to_string();
        if request.url.scheme() != "file" {
            self.remote.fetch(
                doc_id,
                request,
                Box::new(CompletedAssetHandler {
                    handler,
                    requested,
                    waker: self.waker.clone(),
                    doc_id,
                }),
            );
            return;
        }
        let waker = self.waker.clone();
        rayon::spawn(move || {
            // URL paths retain percent escapes and a leading slash before Windows
            // drive letters. Convert the URL instead of passing its path to read.
            let result = request
                .url
                .to_file_path()
                .map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid file URL")
                })
                .and_then(std::fs::read);
            let bytes = match result {
                Ok(bytes) => Bytes::from(bytes),
                Err(error) => {
                    tracing::warn!(url = %request.url, %error, "polymer: loading shell asset failed");
                    // Complete failed stylesheets too, so they cannot leave layout
                    // waiting permanently for a critical resource.
                    Bytes::new()
                }
            };
            handler.bytes(requested, bytes);
            waker.wake(doc_id);
        });
    }
}

pub(super) struct NativeShellProvider {
    proxy: crate::window_manager::EventLoopProxy<crate::app::UserEvent>,
    shell: crate::shell::protocol::ShellInbox,
    clipboard: Mutex<Option<arboard::Clipboard>>,
}

impl NativeShellProvider {
    pub(super) fn new(
        proxy: crate::window_manager::EventLoopProxy<crate::app::UserEvent>,
        shell: crate::shell::protocol::ShellInbox,
    ) -> Self {
        Self {
            proxy,
            shell,
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
        self.shell.wake();
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
            .as_mut()
            .ok_or(ClipboardError)?
            .get_text()
            .map_err(|_| ClipboardError)
    }

    fn set_clipboard_text(&self, text: String) -> Result<(), ClipboardError> {
        self.clipboard
            .lock()
            .as_mut()
            .ok_or(ClipboardError)?
            .set_text(text)
            .map_err(|_| ClipboardError)
    }
}
