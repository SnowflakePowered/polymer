use super::{DEVTOOLS_FRONTEND_BASE, elements, inspector_target};
use objc2::{MainThreadMarker, rc::Retained};
use objc2_app_kit::{NSAutoresizingMaskOptions, NSView};
use objc2_foundation::{NSString, NSURL, NSURLRequest};
use objc2_web_kit::{WKWebView, WKWebViewConfiguration};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{cell::Cell, error::Error, net::SocketAddr};
use winit::{
    event::WindowEvent,
    window::{Window, WindowId},
};
pub(crate) struct InspectorWindow {
    view: Retained<WKWebView>,
    window: Window,
    bridge: elements::Bridge,
    revision: Cell<Option<u64>>,
}
impl InspectorWindow {
    pub(crate) fn open(
        window: Window,
        address: SocketAddr,
        shell: crate::shell::protocol::ShellInbox,
    ) -> Result<Self, Box<dyn Error>> {
        let target = inspector_target(address)?;
        let bridge = elements::Bridge::start(target.web_socket_debugger_url, target.url, shell)?;
        let main = MainThreadMarker::new().ok_or("inspector requires the AppKit thread")?;
        let RawWindowHandle::AppKit(handle) = window.window_handle()?.as_raw() else {
            return Err("inspector requires an AppKit window".into());
        };
        let parent = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
        let configuration = unsafe { WKWebViewConfiguration::new(main) };
        let view = unsafe {
            WKWebView::initWithFrame_configuration(main.alloc(), parent.bounds(), &configuration)
        };
        view.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        parent.addSubview(&view);
        let url = NSURL::URLWithString(&NSString::from_str(&format!(
            "{DEVTOOLS_FRONTEND_BASE}?ws={}&experiments=true",
            bridge.websocket_target()
        )))
        .ok_or("invalid inspector URL")?;
        unsafe {
            view.loadRequest(&NSURLRequest::requestWithURL(&url));
        }
        window.set_visible(true);
        window.focus_window();
        Ok(Self {
            view,
            window,
            bridge,
            revision: Cell::new(None),
        })
    }
    pub(crate) fn show(&self) {
        self.window.set_minimized(false);
        self.window.set_visible(true);
        self.window.focus_window();
    }
    pub(crate) fn id(&self) -> WindowId {
        self.window.id()
    }
    pub(crate) fn handle_window_event(&self, event: &WindowEvent) {
        if matches!(event, WindowEvent::CloseRequested) {
            self.window.set_visible(false);
        }
    }
    pub(crate) fn document_updated(&self, revision: u64) {
        if self
            .revision
            .replace(Some(revision))
            .is_some_and(|old| old != revision)
        {
            self.bridge.document_updated();
        }
    }
}
impl Drop for InspectorWindow {
    fn drop(&mut self) {
        self.view.removeFromSuperview();
    }
}
