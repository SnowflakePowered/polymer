use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, WindowHandle,
};
use snowglass_renderer::{
    D3d12Compositor, Extent, RenderWindow, Renderer, VulkanCompositor,
    capture::{Capture, CaptureEvent, ConsumerPreference},
};
use std::{collections::HashSet, error::Error};
use tauri::WebviewWindow;

#[derive(Clone)]
pub(crate) struct TauriRenderWindow(WebviewWindow);

impl TauriRenderWindow {
    pub(crate) fn new(window: WebviewWindow) -> Self {
        Self(window)
    }
}

impl HasWindowHandle for TauriRenderWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        self.0.window_handle()
    }
}

impl HasDisplayHandle for TauriRenderWindow {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        self.0.display_handle()
    }
}

impl RenderWindow for TauriRenderWindow {
    fn render_extent(&self) -> Extent {
        self.0
            .inner_size()
            .map(|size| Extent::new(size.width, size.height))
            .unwrap_or_default()
    }
}

enum FrontendRenderer {
    D3d12(Box<D3d12Compositor>),
    Vulkan(Box<VulkanCompositor>),
}

impl FrontendRenderer {
    fn new(
        window: &TauriRenderWindow,
        preference: ConsumerPreference,
    ) -> Result<Self, Box<dyn Error>> {
        match preference {
            ConsumerPreference::D3d12 => Ok(Self::D3d12(Box::new(D3d12Compositor::new_default(
                window, false,
            )?))),
            ConsumerPreference::Vulkan => Ok(Self::Vulkan(Box::new(VulkanCompositor::new(
                window, None, true, false,
            )?))),
        }
    }

    fn import_stream(
        &mut self,
        connection: u64,
        stream: &snowglass_ipc::StreamReady,
    ) -> Result<(), Box<dyn Error>> {
        match self {
            Self::D3d12(renderer) => {
                Renderer::import_stream(renderer.as_mut(), connection, stream, 0)
            }
            Self::Vulkan(renderer) => {
                Renderer::import_stream(renderer.as_mut(), connection, stream, 0)
            }
        }
    }

    fn disconnect_stream(&mut self, connection: u64) {
        let result = match self {
            Self::D3d12(renderer) => Renderer::disconnect_stream(renderer.as_mut(), connection),
            Self::Vulkan(renderer) => Renderer::disconnect_stream(renderer.as_mut(), connection),
        };
        if let Err(error) = result {
            eprintln!("snowglass: frontend stream disconnect failed: {error}");
        }
    }

    fn set_latest(&mut self, connection: u64, frame: snowglass_protocol::FrameReady) {
        match self {
            Self::D3d12(renderer) => Renderer::set_latest(renderer.as_mut(), connection, frame),
            Self::Vulkan(renderer) => Renderer::set_latest(renderer.as_mut(), connection, frame),
        }
    }

    fn recreate_swapchain(&mut self, window: &TauriRenderWindow) -> Result<bool, Box<dyn Error>> {
        match self {
            Self::D3d12(renderer) => Renderer::recreate_swapchain(renderer.as_mut(), window),
            Self::Vulkan(renderer) => Renderer::recreate_swapchain(renderer.as_mut(), window),
        }
    }

    fn draw(&mut self) -> Result<bool, Box<dyn Error>> {
        match self {
            Self::D3d12(renderer) => Renderer::draw_game(renderer.as_mut()),
            Self::Vulkan(renderer) => Renderer::draw_game(renderer.as_mut()),
        }
    }
}

pub(crate) struct CaptureLifecycle {
    window: TauriRenderWindow,
    server: Capture,
    renderer: FrontendRenderer,
    active: HashSet<u64>,
}

impl CaptureLifecycle {
    pub(crate) fn new(
        window: WebviewWindow,
        session: &str,
        preference: ConsumerPreference,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, Box<dyn Error>> {
        let window = TauriRenderWindow::new(window);
        Ok(Self {
            renderer: FrontendRenderer::new(&window, preference)?,
            server: Capture::bind_with_waker(session, preference, wake)?,
            window,
            active: HashSet::new(),
        })
    }

    pub(crate) fn process(&mut self) {
        while let Ok(event) = self.server.try_recv() {
            match event {
                CaptureEvent::Stream(connection, stream, accepted) => {
                    let result = self.renderer.import_stream(connection, &stream);
                    let imported = result.is_ok();
                    if imported {
                        self.active.insert(connection);
                    } else if let Err(error) = result {
                        eprintln!("snowglass: frontend rejected capture stream: {error}");
                    }
                    let _ = accepted.send(imported);
                }
                CaptureEvent::Frame(connection, frame) if self.active.contains(&connection) => {
                    self.renderer.set_latest(connection, frame);
                }
                CaptureEvent::Disconnected(connection) => {
                    if self.active.remove(&connection) {
                        self.renderer.disconnect_stream(connection);
                    }
                    self.server.forget(connection);
                }
                CaptureEvent::Frame(_, _) => {}
            }
        }
    }

    pub(crate) fn resize(&mut self) {
        if let Err(error) = self.renderer.recreate_swapchain(&self.window) {
            eprintln!("snowglass: frontend swapchain recreation failed: {error}");
        }
    }

    pub(crate) fn draw(&mut self) {
        if self.active.is_empty() {
            return;
        }
        match self.renderer.draw() {
            Ok(true) => {}
            Ok(false) => self.resize(),
            Err(error) => eprintln!("snowglass: frontend composition failed: {error}"),
        }
    }

    pub(crate) fn shutdown(&mut self) {
        for connection in self.active.drain() {
            self.server.revoke(connection);
            self.renderer.disconnect_stream(connection);
        }
    }
}
