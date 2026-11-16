//! Shared egui input, diagnostics and repaint policy; GPU painting stays in backends.
use polymer_renderer::egui::{
    DebugOverlayModel, InputDiagnostics, RenderTargetExtent, draw_debug_overlay,
};
use polymer_statistics::{FrameMetrics, LiveStatistics, PresentationMetrics, RecorderHandle};
use std::{
    cell::Cell,
    collections::HashMap,
    time::{Duration, Instant},
};
use winit::{event::WindowEvent, window::Window};
pub struct OverlayFrame {
    pub output: egui::FullOutput,
    pub primitives: Vec<egui::ClippedPrimitive>,
}
pub struct OverlayState {
    shell_cursor: Cell<egui::CursorIcon>,
    cursor_dirty: Cell<bool>,
    context: egui::Context,
    state: egui_winit::State,
    diagnostics: DebugOverlayModel,
    frames: HashMap<u64, FrameMetrics>,
    statistics: LiveStatistics,
    presentation: PresentationMetrics,
    frequency: f64,
    next_repaint: Instant,
    interactive_until: Option<Instant>,
    counter: fn() -> std::io::Result<u64>,
}
impl OverlayState {
    pub fn new(
        window: &Window,
        recorder: RecorderHandle,
        consumer: &str,
        backend: &'static str,
        frequency: u64,
        counter: fn() -> std::io::Result<u64>,
    ) -> Self {
        let context = egui::Context::default();
        let state = egui_winit::State::new(
            context.clone(),
            egui::ViewportId::ROOT,
            window,
            Some(window.scale_factor() as f32),
            window.theme(),
            None,
        );
        let diagnostics = DebugOverlayModel {
            backend: "Waiting".to_owned(),
            consumer: consumer.to_owned(),
            ..Default::default()
        };
        Self {
            shell_cursor: Cell::new(egui::CursorIcon::Default),
            cursor_dirty: Cell::new(false),
            context,
            state,
            diagnostics,
            frames: HashMap::new(),
            statistics: LiveStatistics::new(recorder),
            presentation: PresentationMetrics::new(backend),
            frequency: frequency as f64,
            next_repaint: Instant::now(),
            interactive_until: None,
            counter,
        }
    }

    pub fn set_shell_cursor(&self, window: &Window, icon: Option<cursor_icon::CursorIcon>) {
        let cursor = match icon {
            None => egui::CursorIcon::None,
            Some(cursor_icon::CursorIcon::Alias) => egui::CursorIcon::Alias,
            Some(cursor_icon::CursorIcon::AllScroll) => egui::CursorIcon::AllScroll,
            Some(cursor_icon::CursorIcon::Cell) => egui::CursorIcon::Cell,
            Some(cursor_icon::CursorIcon::ContextMenu) => egui::CursorIcon::ContextMenu,
            Some(cursor_icon::CursorIcon::Copy) => egui::CursorIcon::Copy,
            Some(cursor_icon::CursorIcon::Crosshair) => egui::CursorIcon::Crosshair,
            Some(cursor_icon::CursorIcon::Default) => egui::CursorIcon::Default,
            Some(cursor_icon::CursorIcon::Grab) => egui::CursorIcon::Grab,
            Some(cursor_icon::CursorIcon::Grabbing) => egui::CursorIcon::Grabbing,
            Some(cursor_icon::CursorIcon::Help) => egui::CursorIcon::Help,
            Some(cursor_icon::CursorIcon::Move) => egui::CursorIcon::Move,
            Some(cursor_icon::CursorIcon::NoDrop) => egui::CursorIcon::NoDrop,
            Some(cursor_icon::CursorIcon::NotAllowed) => egui::CursorIcon::NotAllowed,
            Some(cursor_icon::CursorIcon::Pointer) => egui::CursorIcon::PointingHand,
            Some(cursor_icon::CursorIcon::Progress) => egui::CursorIcon::Progress,
            Some(cursor_icon::CursorIcon::EwResize) => egui::CursorIcon::ResizeHorizontal,
            Some(cursor_icon::CursorIcon::NeswResize) => egui::CursorIcon::ResizeNeSw,
            Some(cursor_icon::CursorIcon::NwseResize) => egui::CursorIcon::ResizeNwSe,
            Some(cursor_icon::CursorIcon::NsResize) => egui::CursorIcon::ResizeVertical,
            Some(cursor_icon::CursorIcon::EResize) => egui::CursorIcon::ResizeEast,
            Some(cursor_icon::CursorIcon::SeResize) => egui::CursorIcon::ResizeSouthEast,
            Some(cursor_icon::CursorIcon::SResize) => egui::CursorIcon::ResizeSouth,
            Some(cursor_icon::CursorIcon::SwResize) => egui::CursorIcon::ResizeSouthWest,
            Some(cursor_icon::CursorIcon::WResize) => egui::CursorIcon::ResizeWest,
            Some(cursor_icon::CursorIcon::NwResize) => egui::CursorIcon::ResizeNorthWest,
            Some(cursor_icon::CursorIcon::NResize) => egui::CursorIcon::ResizeNorth,
            Some(cursor_icon::CursorIcon::NeResize) => egui::CursorIcon::ResizeNorthEast,
            Some(cursor_icon::CursorIcon::ColResize) => egui::CursorIcon::ResizeColumn,
            Some(cursor_icon::CursorIcon::RowResize) => egui::CursorIcon::ResizeRow,
            Some(cursor_icon::CursorIcon::Text) => egui::CursorIcon::Text,
            Some(cursor_icon::CursorIcon::VerticalText) => egui::CursorIcon::VerticalText,
            Some(cursor_icon::CursorIcon::Wait) => egui::CursorIcon::Wait,
            Some(cursor_icon::CursorIcon::ZoomIn) => egui::CursorIcon::ZoomIn,
            Some(cursor_icon::CursorIcon::ZoomOut) => egui::CursorIcon::ZoomOut,
            _ => egui::CursorIcon::Default,
        };
        if self.shell_cursor.replace(cursor) != cursor {
            self.cursor_dirty.set(true);
            window.request_redraw();
        }
    }

    pub fn set_backend(&mut self, backend: String) {
        self.diagnostics.backend = backend;
    }
    pub fn set_input_diagnostics(&mut self, diagnostics: InputDiagnostics) {
        if self.diagnostics.input != diagnostics {
            self.diagnostics.input = diagnostics;
            self.request_immediate_repaint();
        }
    }
    pub fn add_surface(
        &mut self,
        connection: u64,
        surface: u64,
        target: u32,
        extent: polymer_renderer::Extent,
    ) {
        self.frames
            .insert(connection, FrameMetrics::new(connection, surface, target));
        let render_targets = &mut self.diagnostics.render_targets;
        render_targets.retain(|render_target| render_target.connection != connection);
        render_targets.push(RenderTargetExtent {
            connection,
            target,
            extent,
        });
        render_targets.sort_by_key(|render_target| render_target.target);
    }
    pub fn observe_frame_at(
        &mut self,
        connection: u64,
        frame: polymer_protocol::FrameReady,
        observed_qpc: Option<u64>,
    ) {
        let observed_qpc = observed_qpc.or_else(|| (self.counter)().ok()).unwrap_or(0);
        if let Some(metrics) = self.frames.get_mut(&connection) {
            metrics.observe(frame.frame_number, observed_qpc, self.frequency);
        }
        self.diagnostics.frame_number = frame.frame_number;
        self.diagnostics.slot = frame.slot;
    }
    pub fn record_present(&mut self, carried_new_frame: bool) {
        self.presentation.record_present(carried_new_frame);
    }
    pub fn record_clock(&mut self) {
        self.presentation.record_clock();
    }
    pub fn set_note(&mut self, note: &str) {
        let notes = &mut self.diagnostics.notes;
        if note.is_empty() {
            notes.clear();
        } else if notes.first().is_none_or(|first| first != note) {
            notes.clear();
            notes.push(note.to_owned());
        }
    }
    pub fn remove_target(&mut self, connection: u64) {
        self.frames.remove(&connection);
        self.diagnostics
            .render_targets
            .retain(|render_target| render_target.connection != connection);
        self.statistics.remove_connection(connection);
        self.diagnostics.statistics = self.statistics.snapshot().clone();
    }
    pub fn on_window_event(
        &mut self,
        window: &Window,
        event: &WindowEvent,
    ) -> egui_winit::EventResponse {
        self.state.on_window_event(window, event)
    }
    pub fn note_interaction(&mut self, now: Instant) {
        self.interactive_until = Some(now + EGUI_INTERACTION_GRACE);
        self.next_repaint = self.next_repaint.min(now);
    }
    fn interactive(&self, now: Instant) -> bool {
        self.interactive_until.is_some_and(|until| now < until)
    }
    pub fn repaint_due(&self, now: Instant) -> bool {
        self.cursor_dirty.get() || self.next_repaint <= now
    }
    pub fn next_repaint(&self) -> Instant {
        self.next_repaint
    }
    pub fn request_immediate_repaint(&mut self) {
        self.next_repaint = Instant::now();
    }
    pub fn prepare(&mut self, window: &Window) -> Option<OverlayFrame> {
        let now = Instant::now();
        let interactive = self.interactive(now);
        if !interactive && !self.repaint_due(now) {
            return None;
        }
        self.next_repaint = now + EGUI_STATISTICS_REFRESH;
        self.diagnostics.statistics = self.statistics.sample().clone();
        let input = self.state.take_egui_input(window);
        let diagnostics = &self.diagnostics;
        let mut output = self
            .context
            .run(input, |context| draw_debug_overlay(context, diagnostics));
        // Egui owns the cursor only over its UI (or during a drag). Feed the
        // shell cursor through the same writer/cache when returning to the DOM.
        if !self.context.wants_pointer_input() && !self.context.is_using_pointer() {
            output.platform_output.cursor_icon = self.shell_cursor.get();
        }
        self.cursor_dirty.set(false);
        self.state
            .handle_platform_output(window, std::mem::take(&mut output.platform_output));
        let primitives = self
            .context
            .tessellate(std::mem::take(&mut output.shapes), output.pixels_per_point);
        if interactive {
            let requested = output
                .viewport_output
                .get(&egui::ViewportId::ROOT)
                .map_or(Duration::ZERO, |viewport| viewport.repaint_delay);
            self.next_repaint = now + requested.min(EGUI_STATISTICS_REFRESH);
        }
        if self.context.is_using_pointer() || self.context.wants_pointer_input() {
            self.note_interaction(now);
        }
        Some(OverlayFrame { output, primitives })
    }
}
const EGUI_STATISTICS_REFRESH: Duration = Duration::from_millis(100);
const EGUI_INTERACTION_GRACE: Duration = Duration::from_millis(250);
