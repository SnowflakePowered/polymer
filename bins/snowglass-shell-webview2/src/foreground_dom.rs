//! Foreground WebView2 document and controller ownership.

use crate::{
    input::{DomPointerEvent, DomPointerKind},
    shared_bus::SharedBus,
    window_manager::SurfaceWindow,
};
use std::error::Error;
use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_MOUSE_EVENT_KIND, COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS,
    COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC, ICoreWebView2CompositionController,
};
use windows::{
    Win32::Foundation::POINT,
    Win32::UI::WindowsAndMessaging::{
        WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE,
        WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
    },
    core::{IUnknown, Interface},
};
use wry::{WebView, WebViewBuilder, WebViewBuilderExtWindows, WebViewExtWindows, http::Request};

pub struct ForegroundDom {
    webview: WebView,
    shared_bus: SharedBus,
}

impl ForegroundDom {
    pub fn new(
        window: &SurfaceWindow<'_>,
        width: u32,
        height: u32,
        visual: IUnknown,
        initialization_script: &str,
        url: &str,
        ipc_handler: impl Fn(Request<String>) + 'static,
        navigation_handler: impl Fn(String) + 'static,
    ) -> Result<Self, Box<dyn Error>> {
        let webview = WebViewBuilder::new()
            .with_focused(false)
            .with_transparent(true)
            .with_initialization_script(initialization_script)
            .with_composition_visual_target(visual)
            .with_composition_input_enabled(false)
            .with_ipc_handler(ipc_handler)
            .with_navigation_handler(move |url| {
                navigation_handler(url);
                true
            })
            .build(window)?;
        let shared_bus = SharedBus::new(&webview)?;
        let foreground = Self {
            webview,
            shared_bus,
        };
        foreground.synchronize(width, height)?;
        foreground.webview.load_url(url)?;
        Ok(foreground)
    }

    pub fn synchronize(&self, width: u32, height: u32) -> Result<(), Box<dyn Error>> {
        if width == 0 || height == 0 {
            return Ok(());
        }
        self.webview.set_bounds(wry::Rect {
            position: wry::dpi::PhysicalPosition::new(0, 0).into(),
            size: wry::dpi::PhysicalSize::new(width, height).into(),
        })?;
        // Composition input is routed by the shell instead of Wry's native-window
        // subclass. WebView2 still needs this lifecycle notification to anchor
        // native popup windows such as <select> menus.
        unsafe {
            self.webview
                .controller()
                .NotifyParentWindowPositionChanged()?
        };
        Ok(())
    }

    pub fn focus(&self) -> windows::core::Result<()> {
        unsafe {
            self.webview
                .controller()
                .MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC)
        }
    }

    pub fn send_pointer_event(&self, event: DomPointerEvent) -> windows::core::Result<()> {
        let (kind, mouse_data) = match event.kind {
            DomPointerKind::Move => (WM_MOUSEMOVE, 0),
            DomPointerKind::Button { button, pressed } => (
                match (button, pressed) {
                    (1, true) => WM_LBUTTONDOWN,
                    (1, false) => WM_LBUTTONUP,
                    (2, true) => WM_RBUTTONDOWN,
                    (2, false) => WM_RBUTTONUP,
                    (3, true) => WM_MBUTTONDOWN,
                    (3, false) => WM_MBUTTONUP,
                    (_, true) => WM_XBUTTONDOWN,
                    (_, false) => WM_XBUTTONUP,
                },
                if button >= 4 {
                    u32::from(button - 3)
                } else {
                    0
                },
            ),
            DomPointerKind::Wheel { horizontal, delta } => (
                if horizontal {
                    WM_MOUSEHWHEEL
                } else {
                    WM_MOUSEWHEEL
                },
                delta as u32,
            ),
        };
        let mut virtual_keys = 0_u32;
        for (active, flag) in [
            (event.left, 0x0001),
            (event.right, 0x0002),
            (event.middle, 0x0010),
            (event.back, 0x0020),
            (event.forward, 0x0040),
            (event.shift, 0x0004),
            (event.control, 0x0008),
        ] {
            if active {
                virtual_keys |= flag;
            }
        }
        let controller = self
            .webview
            .controller()
            .cast::<ICoreWebView2CompositionController>()?;
        unsafe {
            controller.SendMouseInput(
                COREWEBVIEW2_MOUSE_EVENT_KIND(kind as i32),
                COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS(virtual_keys as i32),
                mouse_data,
                POINT {
                    x: event.position.x,
                    y: event.position.y,
                },
            )
        }
    }

    pub fn background_bus_endpoint(
        &self,
        wake_native: impl Fn() + Send + Sync + 'static,
    ) -> crate::shared_bus::BackgroundBusEndpoint {
        self.shared_bus.background_endpoint(wake_native)
    }

    pub fn attach_shared_bus(&self) -> windows::core::Result<()> {
        self.shared_bus.attach_foreground(&self.webview)
    }

    pub fn ring_bus_doorbell(&self) -> windows::core::Result<()> {
        self.shared_bus.wake_foreground(&self.webview)
    }

    pub fn shared_bus(&self) -> &SharedBus {
        &self.shared_bus
    }

    pub fn shared_bus_mut(&mut self) -> &mut SharedBus {
        &mut self.shared_bus
    }

    pub fn next_bus_generation(&mut self) -> u32 {
        self.shared_bus.next_generation()
    }
}
