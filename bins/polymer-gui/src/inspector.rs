//! Native WebView2 host for the Deno/Blitz inspector frontend.

#![allow(unsafe_code)]

pub(crate) mod elements;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use serde::Deserialize;
use std::{
    cell::{Cell, RefCell},
    error::Error,
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    rc::Rc,
    time::Duration,
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InspectorTarget {
    web_socket_debugger_url: String,
    url: String,
}

// WebView2 cannot navigate Chromium's internal `devtools://` scheme. Use the
// matching hosted frontend from the WebView2 runtime's Chromium generation.
const DEVTOOLS_FRONTEND_BASE: &str = concat!(
    "https://chrome-devtools-frontend.appspot.com/serve_rev/",
    "@3edf00b60c60c9248990a39a702ca4882809fe06/inspector.html",
);

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

fn inspector_target(address: SocketAddr) -> Result<InspectorTarget, Box<dyn Error>> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(1))?;
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    stream.write_all(
        format!("GET /json HTTP/1.0\r\nHost: {address}\r\nConnection: close\r\n\r\n").as_bytes(),
    )?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    let body = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|offset| &response[offset + 4..])
        .ok_or("Deno inspector returned an invalid HTTP response")?;
    let targets: Vec<InspectorTarget> = serde_json::from_slice(body)?;
    targets
        .into_iter()
        .next()
        .ok_or_else(|| "Deno inspector has no active Blitz target".into())
}
