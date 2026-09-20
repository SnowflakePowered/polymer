//! Native WebView2 host for the Deno/Blitz inspector frontend.

use super::{DEVTOOLS_FRONTEND_BASE, elements, inspector_target};

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{
    cell::{Cell, RefCell},
    error::Error,
    net::SocketAddr,
    rc::Rc,
};
use webview2_com::{
    CoTaskMemPWSTR, CreateCoreWebView2ControllerCompletedHandler,
    CreateCoreWebView2EnvironmentCompletedHandler,
    Microsoft::Web::WebView2::Win32::{CreateCoreWebView2Environment, ICoreWebView2Controller},
};
use windows::{
    Win32::Foundation::{E_POINTER, HWND, RECT},
    core::{Error as WindowsError, HRESULT},
};
use winit::{
    dpi::PhysicalSize,
    event::WindowEvent,
    window::{Window, WindowId},
};

pub(crate) struct InspectorWindow {
    window: Window,
    controller: Rc<RefCell<Option<ICoreWebView2Controller>>>,
    client_size: Rc<Cell<PhysicalSize<u32>>>,
    _elements: elements::Bridge,
    document_revision: Cell<Option<u64>>,
}

impl InspectorWindow {
    pub(crate) fn open(
        window: Window,
        inspector_address: SocketAddr,
        document: std::rc::Weak<RefCell<crate::shell_document::ShellDocument>>,
        proxy: crate::window_manager::EventLoopProxy<crate::app::UserEvent>,
    ) -> Result<Self, Box<dyn Error>> {
        let target = inspector_target(inspector_address)?;
        let elements =
            elements::Bridge::start(target.web_socket_debugger_url, target.url, document, proxy)?;
        let frontend = format!(
            "{DEVTOOLS_FRONTEND_BASE}?ws={}&experiments=true",
            elements.websocket_target()
        );
        let hwnd = window_hwnd(&window)?;
        let controller = Rc::new(RefCell::new(None));
        let client_size = Rc::new(Cell::new(window.inner_size()));
        initialize_webview(hwnd, controller.clone(), client_size.clone(), frontend)?;
        window.set_visible(true);
        window.focus_window();
        Ok(Self {
            window,
            controller,
            client_size,
            _elements: elements,
            document_revision: Cell::new(None),
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
        match event {
            WindowEvent::CloseRequested => self.window.set_visible(false),
            WindowEvent::Resized(size) => self.resize(*size),
            WindowEvent::ScaleFactorChanged { .. } => self.resize(self.window.inner_size()),
            _ => {}
        }
    }

    pub(crate) fn document_updated(&self, revision: u64) {
        let previous = self.document_revision.replace(Some(revision));
        if previous.is_some_and(|previous| previous != revision) {
            self._elements.document_updated();
        }
    }

    fn resize(&self, size: PhysicalSize<u32>) {
        self.client_size.set(size);
        if let Some(controller) = self.controller.borrow().as_ref() {
            let _ = unsafe { controller.SetBounds(bounds(size)) };
        }
    }
}

impl Drop for InspectorWindow {
    fn drop(&mut self) {
        if let Some(controller) = self.controller.borrow_mut().take() {
            let _ = unsafe { controller.Close() };
        }
    }
}

fn window_hwnd(window: &Window) -> Result<HWND, Box<dyn Error>> {
    match window.window_handle()?.as_raw() {
        RawWindowHandle::Win32(handle) => Ok(HWND(handle.hwnd.get() as *mut _)),
        _ => Err("the inspector requires a Win32 Winit window".into()),
    }
}

const fn bounds(size: PhysicalSize<u32>) -> RECT {
    RECT {
        left: 0,
        top: 0,
        right: size.width as i32,
        bottom: size.height as i32,
    }
}

fn initialize_webview(
    hwnd: HWND,
    controller_slot: Rc<RefCell<Option<ICoreWebView2Controller>>>,
    client_size: Rc<Cell<PhysicalSize<u32>>>,
    frontend: String,
) -> windows::core::Result<()> {
    let environment_handler = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(
        move |result, environment| {
            result?;
            let environment = environment.ok_or_else(|| WindowsError::from(E_POINTER))?;
            let controller_handler = CreateCoreWebView2ControllerCompletedHandler::create(
                Box::new(move |result, controller| {
                    result?;
                    let controller = controller.ok_or_else(|| WindowsError::from(E_POINTER))?;
                    unsafe {
                        controller.SetBounds(bounds(client_size.get()))?;
                        controller.SetIsVisible(true)?;
                        let webview = controller.CoreWebView2()?;
                        let uri = CoTaskMemPWSTR::from(frontend.as_str());
                        webview.Navigate(*uri.as_ref().as_pcwstr())?;
                        *controller_slot.borrow_mut() = Some(controller);
                    }
                    Ok(())
                }),
            );
            unsafe { environment.CreateCoreWebView2Controller(hwnd, &controller_handler) }
        },
    ));
    unsafe { CreateCoreWebView2Environment(&environment_handler) }
        .map_err(|error| WindowsError::from(HRESULT(error.code().0)))
}
