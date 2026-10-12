//! Windows retained composition backend. Native handles stay in this crate.
#![cfg(windows)]
mod runtime;
mod shell;
mod surfaces;
mod window;
use polymer_composition::*;
use polymer_renderer::ReceivedSurface;
use polymer_renderer::egui::InputDiagnostics;
use polymer_runtime::{RgbaScreenshot, SurfaceFrameReceiver, SurfaceOffer};
pub use runtime::{NativeEventLoop, TimerResolution, WinRtApartment, WorkerWinRtApartment};
use std::{
    cell::RefCell,
    error::Error,
    rc::Rc,
    time::{Duration, Instant},
};
use winit::{event::WindowEvent, window::WindowId};

pub use polymer_renderer_d3d12::ShaderStage;
use shell::{ShellCompositor, ShellRasterizer};
use surfaces::RenderDevice;
use window::WindowManager;

pub struct WindowsCompositor {
    // Retire captured workers and visual references before the native window.
    surfaces: RenderDevice,
    shell: ShellCompositor,
    window: WindowManager,
}
impl WindowsCompositor {
    pub async fn prepare() -> Result<CompositorFactory, Box<dyn Error>> {
        let rasterizer = ShellRasterizer::new().await?;
        Ok(Box::new(move |window, recorder, wake, debug| {
            let window = WindowManager::new(window, recorder, wake.clone(), debug)?;
            let shell = ShellCompositor::new(&window.compositor(), rasterizer, move || {
                wake(WindowCommand::ShellRasterReady);
            })?;
            let surfaces = RenderDevice::new(shell.device_context());
            window.set_shell_root(shell.root_visual())?;
            window.commit()?;
            Ok(Rc::new(RefCell::new(Self {
                surfaces,
                shell,
                window,
            })))
        }))
    }
}
impl polymer_composition::Compositor for WindowsCompositor {
    fn consumer_preference(&self) -> polymer_runtime::ConsumerPreference {
        polymer_runtime::ConsumerPreference::D3d12
    }
    fn id(&self) -> WindowId {
        self.window.id()
    }
    fn size(&self) -> winit::dpi::PhysicalSize<u32> {
        self.window.size()
    }
    fn scale_factor(&self) -> f32 {
        self.window.scale_factor()
    }
    fn request_redraw(&self) {
        self.window.request_redraw()
    }
    fn set_cursor(&self, icon: Option<cursor_icon::CursorIcon>) {
        self.window.set_cursor(icon)
    }
    fn set_title(&self, title: &str) {
        self.window.set_title(title)
    }
    fn set_ime_enabled(&self, enabled: bool) {
        self.window.set_ime_enabled(enabled)
    }
    fn set_ime_cursor_area(&self, x: f32, y: f32, width: f32, height: f32) {
        self.window.set_ime_cursor_area(x, y, width, height)
    }
    fn focus_host(&self) {
        self.window.focus_host()
    }
    fn handle_window_event(&mut self, event: &WindowEvent) -> bool {
        self.window.handle_window_event(event)
    }
    fn note_overlay_interaction(&mut self, now: Instant) {
        self.window.note_overlay_interaction(now)
    }
    fn resize_overlay(&mut self) -> Result<(), Box<dyn Error>> {
        self.window.resize_overlay()
    }
    fn draw_overlay(&mut self) -> Result<bool, Box<dyn Error>> {
        self.window.draw_overlay()
    }
    fn overlay_repaint_due(&self, now: Instant) -> bool {
        self.window.overlay_repaint_due(now)
    }
    fn next_overlay_repaint(&self) -> Option<Instant> {
        self.window.next_overlay_repaint()
    }
    fn request_overlay_repaint(&mut self) {
        self.window.request_overlay_repaint()
    }
    fn set_overlay_note(&mut self, note: &str) {
        self.window.set_overlay_note(note)
    }
    fn set_input_diagnostics(&mut self, diagnostics: InputDiagnostics) {
        self.window.set_input_diagnostics(diagnostics)
    }
    fn set_backend(&mut self, backend: String) {
        self.window.set_backend(backend)
    }
    fn add_surface(
        &mut self,
        connection: u64,
        surface: u64,
        target: u32,
        extent: polymer_renderer::Extent,
    ) {
        self.window.add_surface(connection, surface, target, extent)
    }
    fn observe_frame_at(
        &mut self,
        connection: u64,
        frame: polymer_protocol::FrameReady,
        observed_qpc: Option<u64>,
    ) {
        self.window
            .observe_frame_at(connection, frame, observed_qpc)
    }
    fn remove_target(&mut self, connection: u64) {
        self.window.remove_target(connection)
    }
    fn record_present(&mut self, carried_new_frame: bool) {
        self.window.record_present(carried_new_frame)
    }
    fn record_clock(&mut self) {
        self.window.record_clock()
    }
    fn set_clock_active(&self, active: bool) {
        self.window.set_clock_active(active)
    }
    fn is_pacing(&self) -> bool {
        self.window.is_pacing()
    }
    fn clear_clock_tick(&self) {
        self.window.clear_clock_tick()
    }
    fn owned_window_attributes(
        &self,
        attributes: winit::window::WindowAttributes,
    ) -> winit::window::WindowAttributes {
        use winit::platform::windows::WindowAttributesExtWindows;
        match self.window.native_handle() {
            Ok(handle) => attributes.with_owner_window(handle.0 as isize),
            Err(_) => attributes,
        }
    }
    fn submit_scene(&mut self, scene: CompositionScene) -> Option<u64> {
        self.shell.submit(scene)
    }
    fn publish_scene(&mut self) -> Result<Option<u64>, String> {
        self.shell.draw()
    }
    fn scene_pending(&self) -> bool {
        self.shell.is_pending()
    }
    fn reconcile(
        &mut self,
        layers: &[SurfaceLayer],
        revision: u64,
    ) -> Result<bool, Box<dyn Error>> {
        self.shell.reconcile(layers, revision, &self.surfaces)
    }
    fn commit(&self) -> Result<(), Box<dyn Error>> {
        self.window.commit().map_err(Into::into)
    }
    fn matches_device(&self, offer: &SurfaceOffer) -> bool {
        self.surfaces.matches(offer)
    }
    fn replace_device(&mut self, offer: &SurfaceOffer) -> Result<(), Box<dyn Error>> {
        let context = self.shell.replace_adapter(offer.stream().adapter_luid)?;
        self.surfaces.replace(context)
    }
    fn import_surface(
        &mut self,
        offer: &SurfaceOffer,
        quarter_turns: u8,
        group: Option<u64>,
    ) -> Result<Option<u64>, Box<dyn Error>> {
        let size = self.window.size();
        self.surfaces.import(
            &self.window.surface(size.width.max(1), size.height.max(1)),
            offer,
            quarter_turns,
            group,
        )
    }
    fn arrange_surface_group(&mut self, group: u64, layout: SurfaceGroupLayout) {
        self.surfaces.arrange_group(group, layout);
    }
    fn accepts_detached_frames(&self) -> bool {
        true
    }
    fn attach_surface_frames(
        &mut self,
        connection: u64,
        frames: SurfaceFrameReceiver,
    ) -> Result<(), Box<dyn Error>> {
        self.surfaces.start_worker(connection, frames)
    }
    fn remove_surface(&mut self, connection: u64) {
        self.surfaces.remove(connection);
    }
    fn set_latest(&mut self, connection: u64, frame: polymer_protocol::FrameReady) {
        self.surfaces.set_latest(connection, frame);
    }
    fn received_surfaces(&self, connection: u64) -> Vec<ReceivedSurface> {
        self.surfaces.received(connection)
    }
    fn try_surface_event(&mut self) -> Option<SurfaceFrameEvent> {
        self.surfaces.try_event()
    }
    fn has_pending_frame(&self) -> bool {
        self.surfaces.pending()
    }
    fn present_surfaces(
        &mut self,
        corners: &[(u64, f32)],
        permit_wait: Duration,
    ) -> Result<Option<bool>, String> {
        self.surfaces.draw(corners, permit_wait)
    }
    fn snapshot(&mut self, connection: u64) -> Result<Option<RgbaScreenshot>, String> {
        self.surfaces.snapshot(connection)
    }
    fn open_video(
        &mut self,
        id: u64,
        source: VideoSource,
        sink: VideoSink,
    ) -> Result<(), Box<dyn Error>> {
        self.surfaces
            .open_video(&self.window.surface(1, 1), id, source, sink)
    }
    fn control_video(&mut self, id: u64, control: VideoControl) {
        self.surfaces.control_video(id, control);
    }
    fn close_video(&mut self, id: u64) {
        self.surfaces.close_video(id);
    }
    fn control_shader(&mut self, target: u64, control: ShaderControl) -> bool {
        self.surfaces.control_shader(target, control)
    }
    fn try_shader_content(&mut self) -> Option<ShaderContent> {
        self.surfaces.try_shader_content()
    }
}
