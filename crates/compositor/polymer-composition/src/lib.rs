//! Native window and retained composition boundary for Polymer frontends.
//! Scene descriptions carry no native graphics resources or DOM objects.
mod events;
pub mod overlay;
mod scene;
mod video;
pub use events::*;
use polymer_renderer::ReceivedSurface;
use polymer_renderer::egui::InputDiagnostics;
use polymer_runtime::{RgbaScreenshot, SurfaceFrameReceiver, SurfaceOffer};
use polymer_statistics::RecorderHandle;
pub use scene::*;
use std::{
    cell::RefCell,
    error::Error,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};
pub use video::*;
use winit::{
    event::WindowEvent,
    window::{Window, WindowId},
};

/// Confined to the event-loop thread; backend workers own their own GPU state.
pub type CompositorHandle = Rc<RefCell<dyn Compositor>>;
pub type Wake = Arc<dyn Fn(WindowCommand) -> bool + Send + Sync>;
pub type CompositorFactory =
    Box<dyn FnOnce(Window, RecorderHandle, Wake, bool) -> Result<CompositorHandle, Box<dyn Error>>>;
pub struct SurfaceFrameEvent {
    pub connection: u64,
    pub update: polymer_runtime::SurfaceFrameUpdate,
    pub observed_qpc: u64,
}
/// Owns the window's device domain, imported surfaces, raster publications and pacing.
/// Surface registration follows successful capture acceptance; removing a surface
/// retires its worker and native content before its capture lease is revoked.
pub trait Compositor {
    fn consumer_preference(&self) -> polymer_runtime::ConsumerPreference;
    fn id(&self) -> WindowId;
    fn size(&self) -> winit::dpi::PhysicalSize<u32>;
    fn scale_factor(&self) -> f32;
    fn request_redraw(&self);
    fn set_cursor(&self, icon: Option<cursor_icon::CursorIcon>);
    fn set_title(&self, title: &str);
    fn set_ime_enabled(&self, enabled: bool);
    fn set_ime_cursor_area(&self, x: f32, y: f32, width: f32, height: f32);
    fn focus_host(&self);
    fn handle_window_event(&mut self, event: &WindowEvent) -> bool;
    fn note_overlay_interaction(&mut self, now: Instant);
    fn resize_overlay(&mut self) -> Result<(), Box<dyn Error>>;
    fn draw_overlay(&mut self) -> Result<bool, Box<dyn Error>>;
    fn overlay_repaint_due(&self, now: Instant) -> bool;
    fn next_overlay_repaint(&self) -> Option<Instant>;
    fn request_overlay_repaint(&mut self);
    fn set_overlay_note(&mut self, note: &str);
    fn set_input_diagnostics(&mut self, diagnostics: InputDiagnostics);
    fn set_backend(&mut self, backend: String);
    fn add_surface(
        &mut self,
        connection: u64,
        surface: u64,
        target: u32,
        extent: polymer_renderer::Extent,
    );
    fn observe_frame_at(
        &mut self,
        connection: u64,
        frame: polymer_protocol::FrameReady,
        observed_qpc: Option<u64>,
    );
    fn remove_target(&mut self, connection: u64);
    fn record_present(&mut self, carried_new_frame: bool);
    fn record_clock(&mut self);
    fn set_clock_active(&self, active: bool);
    fn is_pacing(&self) -> bool;
    fn clear_clock_tick(&self);
    fn owned_window_attributes(
        &self,
        attributes: winit::window::WindowAttributes,
    ) -> winit::window::WindowAttributes;
    /// Returns the revision of a queued scene superseded before rasterization.
    fn submit_scene(&mut self, scene: CompositionScene) -> Option<u64>;
    /// Returns a revision only once all raster layers for it have been published.
    fn publish_scene(&mut self) -> Result<Option<u64>, String>;
    fn scene_pending(&self) -> bool;
    fn reconcile(
        &mut self,
        surfaces: &[SurfaceLayer],
        revision: u64,
    ) -> Result<bool, Box<dyn Error>>;
    fn commit(&self) -> Result<(), Box<dyn Error>>;
    fn matches_device(&self, offer: &SurfaceOffer) -> bool;
    fn replace_device(&mut self, offer: &SurfaceOffer) -> Result<(), Box<dyn Error>>;
    fn import_surface(
        &mut self,
        offer: &SurfaceOffer,
        quarter_turns: u8,
    ) -> Result<(), Box<dyn Error>>;
    fn accepts_detached_frames(&self) -> bool;
    fn attach_surface_frames(
        &mut self,
        connection: u64,
        frames: SurfaceFrameReceiver,
    ) -> Result<(), Box<dyn Error>>;
    fn register_surface(&mut self, connection: u64, target: u32) -> Result<(), Box<dyn Error>>;
    fn remove_surface(&mut self, connection: u64);
    fn set_latest(&mut self, connection: u64, frame: polymer_protocol::FrameReady);
    fn received_surfaces(&self, connection: u64) -> Vec<ReceivedSurface>;
    fn try_surface_event(&mut self) -> Option<SurfaceFrameEvent>;
    fn has_pending_frame(&self) -> bool;
    fn present_surfaces(
        &mut self,
        corners: &[(u64, f32)],
        permit_wait: Duration,
    ) -> Result<Option<bool>, String>;
    fn snapshot(&mut self, connection: u64) -> Result<Option<RgbaScreenshot>, String>;
    fn start_webrtc(
        &mut self,
        bind: std::net::SocketAddr,
        targets: &[u32],
        streams: &[(u64, u32)],
    ) -> Result<(), Box<dyn Error>>;
    fn stop_webrtc(&mut self);
    fn open_video(
        &mut self,
        id: u64,
        bytes: VideoBytes,
        sink: VideoSink,
    ) -> Result<(), Box<dyn Error>>;
    fn control_video(&mut self, id: u64, control: VideoControl);
    fn close_video(&mut self, id: u64);
}
