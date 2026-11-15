//! Application event dispatch; native window/composition resources live in the backend.
pub(crate) use polymer_composition::{
    LoopDirective, ShellDeviceEvent, ShellImeEvent, ShellWindowEvent, WindowCommand,
    translate_device_event, translate_window_event,
};
use std::error::Error;
#[cfg(windows)]
use std::time::Instant;
pub(crate) use winit::event_loop::{EventLoop, EventLoopProxy};
#[cfg(windows)]
pub(crate) use winit::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{DeviceEvent, DeviceId, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow},
    window::{Window, WindowId},
};
/// Raw winit integration. The coordinated application receives only neutral
/// shell/device events and an abstract loop directive.
pub(crate) struct EventLoopApplication<'a> {
    native_turn: Option<Box<dyn FnMut(&mut crate::app::ShellApplication) + 'a>>,
    inner: crate::app::ShellApplication,
    creating_window: bool,
    #[cfg(windows)]
    next_wake: Instant,
    #[cfg(windows)]
    wait: fn(Instant) -> Result<(), Box<dyn Error>>,
}

impl<'a> EventLoopApplication<'a> {
    pub fn new(
        inner: crate::app::ShellApplication,
        #[cfg(windows)] wait: fn(Instant) -> Result<(), Box<dyn Error>>,
    ) -> Self {
        Self {
            native_turn: None,
            inner,
            #[cfg(windows)]
            wait,
            creating_window: false,
            #[cfg(windows)]
            next_wake: Instant::now(),
        }
    }

    #[cfg(target_os = "macos")]
    pub fn set_native_turn(&mut self, turn: impl FnMut(&mut crate::app::ShellApplication) + 'a) {
        self.native_turn = Some(Box::new(turn));
    }
    #[cfg(windows)]
    pub async fn run_async_tasks(&mut self) {
        self.inner.run_async_tasks().await;
    }

    #[cfg(windows)]
    pub fn wait_for_events(&self) -> Result<(), Box<dyn Error>> {
        (self.wait)(self.next_wake)
    }

    pub async fn into_result(self) -> Result<(), Box<dyn Error>> {
        self.inner.into_result().await
    }
}

impl ApplicationHandler<crate::app::UserEvent> for EventLoopApplication<'_> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Keep native-window creation in the resume callback. The coordinator
        // consumes it after this event-pump turn returns and starts the UI.
        if self.creating_window || !self.inner.wants_native_window() {
            return;
        }
        self.creating_window = true;
        #[cfg_attr(not(any(windows, target_os = "macos")), allow(unused_variables))]
        let titlebar = self.inner.titlebar();
        let attributes = Window::default_attributes()
            .with_title("Polymer")
            .with_transparent(true)
            .with_inner_size(LogicalSize::new(1280.0, 720.0));
        #[cfg(target_os = "macos")]
        let attributes = {
            use winit::platform::macos::WindowAttributesExtMacOS;
            attributes
                .with_titlebar_transparent(!titlebar)
                .with_title_hidden(!titlebar)
                .with_fullsize_content_view(!titlebar)
        };
        match event_loop.create_window(attributes) {
            Ok(window) => {
                #[cfg(windows)]
                if let Err(error) = window_vibrancy::apply_mica(&window, Some(true)) {
                    tracing::warn!(%error, "polymer: could not apply the Mica backdrop");
                }
                #[cfg(windows)]
                if !titlebar {
                    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
                    let extended = match window.window_handle().map(|handle| handle.as_raw()) {
                        Ok(RawWindowHandle::Win32(handle)) => {
                            polymer_platform_windows::ExtendedTitleBar::install(
                                polymer_platform_windows::NativeWindow(handle.hwnd.get()),
                            )
                        }
                        _ => Err(std::io::Error::other(
                            "the shell window has no Win32 handle",
                        )),
                    };
                    if let Err(error) = extended {
                        tracing::warn!(%error, "polymer: could not extend the shell into the title bar");
                    }
                }
                #[cfg(target_os = "macos")]
                if let Err(error) = window_vibrancy::apply_vibrancy(
                    &window,
                    window_vibrancy::NSVisualEffectMaterial::HudWindow,
                    None,
                    None,
                ) {
                    tracing::warn!(%error, "polymer: could not apply the window vibrancy");
                }
                self.inner.accept_native_window(window)
            }
            Err(error) => {
                self.inner.fail(error);
                event_loop.exit();
            }
        }
        self.creating_window = false;
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: crate::app::UserEvent) {
        if let crate::app::UserEvent::InspectorWindowRequested { address } = event {
            if self.inner.inspector_window_id().is_some() {
                return;
            }
            let attributes = Window::default_attributes()
                .with_title("Polymer Inspector")
                .with_inner_size(LogicalSize::new(1280.0, 800.0))
                .with_visible(false);
            let attributes = self
                .inner
                .compositor_handle()
                .map_or(attributes.clone(), |host| {
                    host.borrow().owned_window_attributes(attributes)
                });
            match event_loop.create_window(attributes) {
                Ok(window) => self.inner.accept_inspector_window(window, address),
                Err(error) => {
                    tracing::error!(%error, "polymer: creating inspector window failed")
                }
            }
            return;
        }
        self.inner.handle_user_event(event);
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: DeviceId,
        event: DeviceEvent,
    ) {
        self.inner
            .handle_device_event(translate_device_event(event));
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if self.inner.inspector_window_id() == Some(window_id) {
            self.inner.handle_inspector_window_event(&event);
            return;
        }
        let Some(manager_handle) = self.inner.compositor_handle() else {
            return;
        };
        let mut manager = manager_handle.borrow_mut();
        if window_id != manager.id() {
            return;
        }
        let overlay_consumed = manager.handle_window_event(&event);
        drop(manager);
        let event = translate_window_event(event);
        if self.inner.handle_window_event(event, overlay_consumed) {
            event_loop.exit();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(turn) = &mut self.native_turn {
            turn(&mut self.inner);
        }
        match self.inner.about_to_wait() {
            LoopDirective::Exit => event_loop.exit(),
            LoopDirective::WaitUntil(deadline) => {
                #[cfg(windows)]
                {
                    self.next_wake = deadline;
                }
                event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
            }
        }
    }
}
