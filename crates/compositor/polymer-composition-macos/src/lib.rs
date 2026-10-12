//! AppKit/Core Animation retained composition, with the shared Metal renderer.
#![cfg(target_os = "macos")]
mod overlay;
mod raster;
mod surfaces;
mod tree;
use objc2_quartz_core::CATransaction;
use overlay::EguiOverlay;
use polymer_composition::*;
use polymer_renderer::egui::InputDiagnostics;
use polymer_renderer::{ReceivedSurface, Renderer};
use polymer_runtime::{RgbaScreenshot, SurfaceFrameReceiver, SurfaceOffer};
use raster::{RasterLayer, Rasterizer};
use std::{
    cell::RefCell,
    collections::HashMap,
    error::Error,
    rc::Rc,
    time::{Duration, Instant},
};
use surfaces::Surfaces;
use tree::LayerTree;
use winit::{
    event::WindowEvent,
    window::{Window, WindowId},
};

pub struct MacosCompositor {
    surfaces: Surfaces,
    rasters: HashMap<RasterChunkId, RasterLayer>,
    rasterizer: Rasterizer,
    overlay: Option<EguiOverlay>,
    tree: LayerTree,
    pending: Option<CompositionScene>,
    published: Option<CompositionScene>,
    window: Window,
    reconciled: Option<(u64, u64, u64)>,
}
impl MacosCompositor {
    pub async fn prepare() -> Result<CompositorFactory, Box<dyn Error>> {
        let mut rasterizer = Rasterizer::new().await?;
        Ok(Box::new(move |window, recorder, wake, debug| {
            let redraw: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(move || {
                wake(WindowCommand::ShellRasterReady);
            });
            rasterizer.wake = Some(redraw.clone());
            let tree = LayerTree::new(&window)?;
            let surfaces = Surfaces::new(rasterizer.native.clone(), redraw.clone());
            let overlay = if debug {
                Some(EguiOverlay::new(
                    &window,
                    recorder,
                    rasterizer.native.clone(),
                    redraw.clone(),
                )?)
            } else {
                None
            };
            let mut value = Self {
                surfaces,
                rasters: HashMap::new(),
                rasterizer,
                overlay,
                tree,
                pending: None,
                published: None,
                window,
                reconciled: None,
            };
            value.resize_overlay()?;
            Ok(Rc::new(RefCell::new(value)))
        }))
    }
}
impl Compositor for MacosCompositor {
    fn consumer_preference(&self) -> polymer_runtime::ConsumerPreference {
        polymer_runtime::ConsumerPreference::Metal
    }
    fn id(&self) -> WindowId {
        self.window.id()
    }
    fn size(&self) -> winit::dpi::PhysicalSize<u32> {
        self.window.inner_size()
    }
    fn scale_factor(&self) -> f32 {
        self.window.scale_factor() as f32
    }
    fn request_redraw(&self) {
        self.window.request_redraw();
    }
    fn set_cursor(&self, icon: Option<cursor_icon::CursorIcon>) {
        if let Some(overlay) = &self.overlay {
            overlay.set_shell_cursor(&self.window, icon);
            return;
        }
        self.window.set_cursor_visible(icon.is_some());
        if let Some(icon) = icon {
            self.window.set_cursor(icon);
        }
    }
    fn set_title(&self, title: &str) {
        self.window.set_title(title);
    }
    fn set_ime_enabled(&self, enabled: bool) {
        self.window.set_ime_allowed(enabled);
    }
    fn set_ime_cursor_area(&self, x: f32, y: f32, width: f32, height: f32) {
        self.window.set_ime_cursor_area(
            winit::dpi::PhysicalPosition::new(x, y),
            winit::dpi::PhysicalSize::new(width, height),
        );
    }
    fn focus_host(&self) {
        self.window.focus_window();
    }
    fn handle_window_event(&mut self, event: &WindowEvent) -> bool {
        self.overlay
            .as_mut()
            .is_some_and(|o| o.on_window_event(&self.window, event).consumed)
    }
    fn note_overlay_interaction(&mut self, now: Instant) {
        if let Some(o) = &mut self.overlay {
            o.note_interaction(now);
        }
    }
    fn resize_overlay(&mut self) -> Result<(), Box<dyn Error>> {
        if let Some(o) = &mut self.overlay {
            o.resize(&self.window)?;
            let size = self.window.inner_size();
            CATransaction::begin();
            CATransaction::setDisableActions(true);
            LayerTree::place(
                o.layer(),
                &self.tree.root,
                [0., 0., size.width as f64, size.height as f64],
                i32::MAX,
                self.window.scale_factor(),
            );
            CATransaction::commit();
        }
        Ok(())
    }
    fn draw_overlay(&mut self) -> Result<bool, Box<dyn Error>> {
        self.overlay
            .as_mut()
            .map_or(Ok(false), |o| o.draw(&self.window))
    }
    fn overlay_repaint_due(&self, now: Instant) -> bool {
        self.overlay.as_ref().is_some_and(|o| o.repaint_due(now))
    }
    fn next_overlay_repaint(&self) -> Option<Instant> {
        self.overlay.as_ref().map(|o| o.next_repaint())
    }
    fn request_overlay_repaint(&mut self) {
        if let Some(o) = &mut self.overlay {
            o.request_immediate_repaint();
        }
    }
    fn set_overlay_note(&mut self, note: &str) {
        if let Some(o) = &mut self.overlay {
            o.set_note(note);
        }
    }
    fn set_input_diagnostics(&mut self, diagnostics: InputDiagnostics) {
        if let Some(o) = &mut self.overlay {
            o.set_input_diagnostics(diagnostics);
        }
    }
    fn set_backend(&mut self, backend: String) {
        if let Some(o) = &mut self.overlay {
            o.set_backend(backend);
        }
    }
    fn add_surface(
        &mut self,
        connection: u64,
        surface: u64,
        target: u32,
        extent: polymer_renderer::Extent,
    ) {
        if let Some(o) = &mut self.overlay {
            o.add_surface(connection, surface, target, extent);
        }
    }
    fn observe_frame_at(
        &mut self,
        connection: u64,
        frame: polymer_protocol::FrameReady,
        observed_qpc: Option<u64>,
    ) {
        if let Some(o) = &mut self.overlay {
            o.observe_frame_at(connection, frame, observed_qpc);
        }
    }
    fn remove_target(&mut self, connection: u64) {
        if let Some(o) = &mut self.overlay {
            o.remove_target(connection);
        }
    }
    fn record_present(&mut self, carried_new_frame: bool) {
        if let Some(o) = &mut self.overlay {
            o.record_present(carried_new_frame);
        }
    }
    fn record_clock(&mut self) {
        if let Some(o) = &mut self.overlay {
            o.record_clock();
        }
    }
    // CAMetalLayer throttles submissions to display refresh. There is no separate
    // compositor tick source, so the frontend uses its unpaced submission branch.
    fn set_clock_active(&self, active: bool) {
        if active {
            self.window.request_redraw();
        }
    }
    fn is_pacing(&self) -> bool {
        false
    }
    fn clear_clock_tick(&self) {}
    fn owned_window_attributes(
        &self,
        attributes: winit::window::WindowAttributes,
    ) -> winit::window::WindowAttributes {
        attributes
    }
    fn submit_scene(&mut self, scene: CompositionScene) -> Option<u64> {
        self.window.request_redraw();
        self.pending.replace(scene).map(|s| s.revision)
    }
    fn publish_scene(&mut self) -> Result<Option<u64>, String> {
        let Some(scene) = self.pending.as_ref() else {
            return Ok(None);
        };
        LayerTree::validate(scene).map_err(|e| e.to_string())?;
        if !self
            .rasterizer
            .render(scene, &mut self.rasters)
            .map_err(|e| e.to_string())?
        {
            return Ok(None);
        }
        let revision = scene.revision;
        self.published = self.pending.take();
        Ok(Some(revision))
    }
    fn scene_pending(&self) -> bool {
        self.pending.is_some() && !self.rasterizer.waiting()
    }
    fn reconcile(
        &mut self,
        surfaces: &[SurfaceLayer],
        revision: u64,
    ) -> Result<bool, Box<dyn Error>> {
        let key = (
            revision,
            self.published.as_ref().map_or(0, |s| s.revision),
            self.window.scale_factor().to_bits(),
        );
        if self.reconciled == Some(key) {
            return Ok(false);
        }
        self.tree.reconcile(
            self.published.as_ref(),
            &self.rasters,
            surfaces,
            &self.surfaces,
            self.window.scale_factor(),
        );
        self.reconciled = Some(key);
        Ok(true)
    }
    fn commit(&self) -> Result<(), Box<dyn Error>> {
        self.tree.commit();
        Ok(())
    }
    fn matches_device(&self, offer: &SurfaceOffer) -> bool {
        offer.metal_resources().is_some()
    }
    fn replace_device(&mut self, offer: &SurfaceOffer) -> Result<(), Box<dyn Error>> {
        if !self.matches_device(offer) {
            return Err("macOS composition requires IOSurface/Metal shared-event resources".into());
        }
        self.surfaces.clear();
        self.reconciled = None;
        Ok(())
    }
    fn import_surface(
        &mut self,
        offer: &SurfaceOffer,
        quarter_turns: u8,
    ) -> Result<(), Box<dyn Error>> {
        self.reconciled = None;
        self.surfaces.import(offer, quarter_turns)
    }
    fn accepts_detached_frames(&self) -> bool {
        false
    }
    fn attach_surface_frames(
        &mut self,
        _connection: u64,
        _frames: SurfaceFrameReceiver,
    ) -> Result<(), Box<dyn Error>> {
        Err("Metal composition receives frames through the frontend capture dispatcher".into())
    }
    fn register_surface(&mut self, connection: u64, target: u32) -> Result<(), Box<dyn Error>> {
        self.surfaces.register(connection, target)
    }
    fn remove_surface(&mut self, connection: u64) {
        self.reconciled = None;
        self.surfaces.remove(connection);
    }
    fn set_latest(&mut self, connection: u64, frame: polymer_protocol::FrameReady) {
        if let Some(s) = self.surfaces.streams.get_mut(&connection) {
            s.renderer.set_latest(connection, frame);
        }
    }
    fn received_surfaces(&self, connection: u64) -> Vec<ReceivedSurface> {
        self.surfaces
            .streams
            .get(&connection)
            .map_or_else(Vec::new, |s| s.renderer.received_surfaces())
    }
    fn try_surface_event(&mut self) -> Option<SurfaceFrameEvent> {
        None
    }
    fn has_pending_frame(&self) -> bool {
        self.surfaces
            .streams
            .values()
            .any(|s| s.renderer.has_pending_frame())
    }
    fn present_surfaces(
        &mut self,
        corners: &[(u64, f32)],
        _permit_wait: Duration,
    ) -> Result<Option<bool>, String> {
        self.surfaces.present(corners, self.window.scale_factor())
    }
    fn snapshot(&mut self, connection: u64) -> Result<Option<RgbaScreenshot>, String> {
        self.surfaces
            .streams
            .get(&connection)
            .map_or(Ok(None), |s| {
                s.renderer.snapshot(connection).map_err(|e| e.to_string())
            })
    }
    fn start_webrtc(
        &mut self,
        bind: std::net::SocketAddr,
        targets: &[u32],
        streams: &[(u64, u32)],
    ) -> Result<(), Box<dyn Error>> {
        self.surfaces.start_webrtc(bind, targets, streams)
    }
    fn stop_webrtc(&mut self) {
        self.surfaces.stop_webrtc();
    }
    fn open_video(
        &mut self,
        _id: u64,
        _source: VideoSource,
        _sink: VideoSink,
    ) -> Result<(), Box<dyn Error>> {
        Err("video playback is not supported on macOS yet".into())
    }
    fn control_video(&mut self, _id: u64, _control: VideoControl) {}
    fn close_video(&mut self, _id: u64) {}
}
