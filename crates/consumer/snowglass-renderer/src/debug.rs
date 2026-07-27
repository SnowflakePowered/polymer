//! Reusable egui diagnostics shared by native Snowglass hosts.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
struct StreamSequence {
    frame_number: u64,
    timestamp_qpc: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameSequenceObservation {
    pub interval_qpc: Option<f64>,
    pub dropped: u64,
}

/// Tracks producer frame sequences independently for each capture connection.
///
/// Frame numbers and capture timestamps are local to a producer stream. Mixing
/// them across connections makes two interleaved render targets look like frame
/// gaps and produces meaningless frame intervals.
#[derive(Clone, Debug, Default)]
pub struct FrameSequenceTracker {
    streams: HashMap<u64, StreamSequence>,
    dropped: u64,
}

impl FrameSequenceTracker {
    pub fn observe(
        &mut self,
        connection: u64,
        frame_number: u64,
        timestamp_qpc: u64,
    ) -> FrameSequenceObservation {
        let Some(previous) = self.streams.get_mut(&connection) else {
            self.streams.insert(
                connection,
                StreamSequence {
                    frame_number,
                    timestamp_qpc,
                },
            );
            return FrameSequenceObservation::default();
        };

        if frame_number == previous.frame_number {
            return FrameSequenceObservation::default();
        }
        if frame_number < previous.frame_number {
            *previous = StreamSequence {
                frame_number,
                timestamp_qpc,
            };
            return FrameSequenceObservation::default();
        }

        let frame_delta = frame_number - previous.frame_number;
        let interval_qpc = timestamp_qpc
            .checked_sub(previous.timestamp_qpc)
            .map(|ticks| ticks as f64 / frame_delta as f64);
        let dropped = frame_delta - 1;
        self.dropped += dropped;
        *previous = StreamSequence {
            frame_number,
            timestamp_qpc,
        };
        FrameSequenceObservation {
            interval_qpc,
            dropped,
        }
    }

    pub fn remove(&mut self, connection: u64) {
        self.streams.remove(&connection);
    }

    pub fn stream_count(&self) -> usize {
        self.streams.len()
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

pub const STATISTICS_SAMPLE_INTERVAL: Duration = Duration::from_millis(500);
pub const STATISTICS_ROLLING_WINDOW: Duration = Duration::from_secs(120);

#[derive(Clone, Debug, Default)]
pub struct TargetStatisticsDiagnostics {
    pub target: u32,
    pub fps: f64,
    pub latency_ms: f64,
    pub frame_interval_ms: f64,
    pub history: VecDeque<StatisticsSample>,
}

#[derive(Clone, Debug)]
struct TargetStatisticsState {
    target: u32,
    frames: u64,
    sample_started: Instant,
    fps: f64,
    latency_ms: f64,
    frame_interval_ms: f64,
    history: VecDeque<StatisticsSample>,
}

/// Samples FPS, latency, and frame time independently for every render target.
#[derive(Clone, Debug, Default)]
pub struct TargetStatisticsTracker {
    targets: HashMap<u64, TargetStatisticsState>,
}

impl TargetStatisticsTracker {
    pub fn observe(
        &mut self,
        connection: u64,
        target: u32,
        latency_ms: f64,
        frame_interval_ms: Option<f64>,
    ) {
        self.observe_at(
            connection,
            target,
            latency_ms,
            frame_interval_ms,
            Instant::now(),
        );
    }

    fn observe_at(
        &mut self,
        connection: u64,
        target: u32,
        latency_ms: f64,
        frame_interval_ms: Option<f64>,
        now: Instant,
    ) {
        let state = self
            .targets
            .entry(connection)
            .or_insert(TargetStatisticsState {
                target,
                frames: 0,
                sample_started: now,
                fps: 0.0,
                latency_ms,
                frame_interval_ms: frame_interval_ms.unwrap_or(0.0),
                history: VecDeque::new(),
            });
        state.target = target;
        state.frames += 1;
        state.latency_ms = latency_ms;
        if let Some(frame_interval_ms) = frame_interval_ms {
            state.frame_interval_ms = frame_interval_ms;
        }
    }

    pub fn sample(&mut self) -> bool {
        self.sample_at(Instant::now())
    }

    fn sample_at(&mut self, now: Instant) -> bool {
        let mut changed = false;
        for state in self.targets.values_mut() {
            let elapsed = now.saturating_duration_since(state.sample_started);
            if elapsed < STATISTICS_SAMPLE_INTERVAL {
                continue;
            }
            state.fps = state.frames as f64 / elapsed.as_secs_f64();
            state.frames = 0;
            state.sample_started = now;
            state.history.push_back(StatisticsSample {
                sampled_at: now,
                fps: state.fps as f32,
                latency_ms: state.latency_ms as f32,
                frame_interval_ms: state.frame_interval_ms as f32,
            });
            while state.history.front().is_some_and(|sample| {
                now.saturating_duration_since(sample.sampled_at) > STATISTICS_ROLLING_WINDOW
            }) {
                state.history.pop_front();
            }
            changed = true;
        }
        changed
    }

    pub fn remove(&mut self, connection: u64) {
        self.targets.remove(&connection);
    }

    pub fn statistics(&self) -> Vec<TargetStatisticsDiagnostics> {
        let mut statistics = self
            .targets
            .values()
            .map(|state| TargetStatisticsDiagnostics {
                target: state.target,
                fps: state.fps,
                latency_ms: state.latency_ms,
                frame_interval_ms: state.frame_interval_ms,
                history: state.history.clone(),
            })
            .collect::<Vec<_>>();
        statistics.sort_by_key(|statistics| statistics.target);
        statistics
    }
}

#[derive(Clone, Copy, Debug)]
pub struct StatisticsSample {
    pub sampled_at: Instant,
    pub fps: f32,
    pub latency_ms: f32,
    pub frame_interval_ms: f32,
}

/// Presentation rate against the display clock that is supposed to be pacing
/// it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PacingRates {
    pub presented: f64,
    pub clock: f64,
}

/// Counts presents against composition-clock ticks.
///
/// A host that presents only when content changes cannot be judged by how often
/// it repeats a frame, the way one presenting every refresh can: it never
/// repeats. What matters instead is whether each composition interval received
/// exactly one present. Presents landing at the clock's rate means every
/// interval got a frame; falling short means intervals went by showing what
/// they already had.
#[derive(Clone, Debug, Default)]
pub struct PacingTracker {
    presented: u64,
    clock: u64,
    started: Option<Instant>,
    rates: PacingRates,
}

impl PacingTracker {
    pub const fn record_present(&mut self) {
        self.presented += 1;
    }

    pub const fn record_clock(&mut self) {
        self.clock += 1;
    }

    pub fn rates(&mut self) -> PacingRates {
        self.rates_at(Instant::now())
    }

    fn rates_at(&mut self, now: Instant) -> PacingRates {
        let started = *self.started.get_or_insert(now);
        let elapsed = now.saturating_duration_since(started);
        if elapsed < STATISTICS_SAMPLE_INTERVAL {
            return self.rates;
        }
        self.rates = PacingRates {
            presented: self.presented as f64 / elapsed.as_secs_f64(),
            clock: self.clock as f64 / elapsed.as_secs_f64(),
        };
        self.presented = 0;
        self.clock = 0;
        self.started = Some(now);
        self.rates
    }
}

#[derive(Clone, Debug, Default)]
pub struct FrameDiagnostics {
    pub backend: String,
    pub consumer: String,
    /// Zero until a host reports composition-clock pacing, which is what keeps
    /// this out of overlays whose host does not have a clock to report.
    pub pacing: PacingRates,
    /// Host-specific lines appended to the panel. What limits a host's frame
    /// rate is particular to how it is built, so the shared overlay carries
    /// them rather than modelling any one host's internals.
    pub notes: Vec<String>,
    /// Guest capture to scanout, when the backend can attribute presents to
    /// the vblank that displayed them.
    pub display_latency: Option<crate::renderer::DisplayLatencyReport>,
    pub frame_number: u64,
    pub slot: u32,
    pub render_width: u32,
    pub render_height: u32,
    pub capture_x: u32,
    pub capture_y: u32,
    pub capture_width: u32,
    pub capture_height: u32,
    pub dropped: u64,
    pub target_statistics: Vec<TargetStatisticsDiagnostics>,
}

#[derive(Clone, Debug, Default)]
pub struct InputAxisDiagnostics {
    pub name: String,
    pub value: i16,
}

#[derive(Clone, Debug, Default)]
pub struct InputSlotDiagnostics {
    pub slot: usize,
    pub layout: String,
    pub connected: bool,
    pub name: String,
    pub guid: String,
    pub pressed: Vec<String>,
    pub axes: Vec<InputAxisDiagnostics>,
    pub mapped_buttons: u32,
    pub left_stick: [i16; 2],
    pub right_stick: [i16; 2],
    pub left_trigger: u16,
    pub right_trigger: u16,
    pub pointer: [i16; 2],
    pub accelerometer: [f32; 3],
    pub gyroscope: [f32; 3],
}

#[derive(Clone, Debug, Default)]
pub struct InputDiagnostics {
    pub active: bool,
    pub dispatch_samples: usize,
    pub dispatch_p50_ms: f64,
    pub dispatch_p95_ms: f64,
    pub dispatch_p99_ms: f64,
    pub slots: Vec<InputSlotDiagnostics>,
}

#[derive(Clone, Debug, Default)]
pub struct DebugOverlay {
    pub frame: FrameDiagnostics,
    pub input: InputDiagnostics,
}

pub fn draw_debug_overlay(context: &egui::Context, diagnostics: &DebugOverlay) {
    egui::TopBottomPanel::top("snowglass-debug-statistics").show(context, |ui| {
        ui.horizontal_wrapped(|ui| {
            let frame = &diagnostics.frame;
            ui.strong(format!(
                "Snowglass {} via {}",
                frame.backend, frame.consumer
            ));
            ui.label(format!("Frame {}", frame.frame_number));
            ui.label(format!("Slot {}", frame.slot));
            ui.label(format!(
                "Render target {}x{}",
                frame.render_width, frame.render_height
            ));
            ui.label(format!(
                "Scissor ({}, {}) {}x{}",
                frame.capture_x, frame.capture_y, frame.capture_width, frame.capture_height
            ));
            if frame.target_statistics.is_empty() {
                ui.label("Target statistics waiting");
            } else {
                for target in &frame.target_statistics {
                    ui.label(format!(
                        "Target {}: {:.1} FPS · latency {:.2} ms · frame Δ {:.2} ms",
                        target.target, target.fps, target.latency_ms, target.frame_interval_ms,
                    ));
                }
            }
            ui.label(format!("Dropped {}", frame.dropped));
            if frame.pacing.clock > 0.0 {
                let missed = (frame.pacing.clock - frame.pacing.presented).max(0.0);
                ui.colored_label(
                    if missed > frame.pacing.clock * 0.05 {
                        egui::Color32::LIGHT_RED
                    } else {
                        egui::Color32::LIGHT_GREEN
                    },
                    format!(
                        "Presented {:.1}/s · clock {:.1}/s",
                        frame.pacing.presented, frame.pacing.clock
                    ),
                )
                .on_hover_text(
                    "Presents per second against composition-clock ticks per second. \
                     Equal rates mean every composition interval received a frame; \
                     presenting short of the clock means intervals went by still \
                     showing the frame before.",
                );
            }
            if let Some(latency) = frame.display_latency {
                ui.colored_label(
                    egui::Color32::LIGHT_BLUE,
                    format!(
                        "To display {:.1} ms · worst {:.1} ms",
                        latency.average_ms, latency.worst_ms
                    ),
                )
                .on_hover_text(format!(
                    "Guest capture to the vblank that scanned the frame out, \
                     measured on one clock: transport, the wait for a \
                     composition pass, and the pass itself. Panel response is \
                     not included. {} present(s) confirmed in the last window.",
                    latency.confirmed
                ));
            }
            for note in &frame.notes {
                ui.monospace(note);
            }
        });
    });

    let transparent_frame = egui::Frame::window(&context.style())
        .fill(egui::Color32::TRANSPARENT)
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_white_alpha(48)))
        .shadow(egui::epaint::Shadow::NONE);
    egui::Window::new("Frame Graphs · rolling 120 seconds")
        .default_pos(egui::pos2(12.0, 48.0))
        .resizable(false)
        .collapsible(true)
        .frame(transparent_frame)
        .show(context, |ui| {
            if diagnostics.frame.target_statistics.is_empty() {
                ui.label("Collecting per-target frame statistics…");
            }
            for target in &diagnostics.frame.target_statistics {
                ui.strong(format!("Target {}", target.target));
                draw_statistics_graph(ui, &target.history);
            }
        });
    draw_input_debug_window(context, &diagnostics.input);
}

pub fn draw_input_debug_window(context: &egui::Context, input: &InputDiagnostics) {
    egui::Window::new("Input Debug")
        .default_pos(egui::pos2(880.0, 48.0))
        .default_width(380.0)
        .show(context, |ui| {
            if input.dispatch_samples == 0 {
                ui.colored_label(egui::Color32::GRAY, "Input dispatch latency: waiting");
            } else {
                ui.label(format!(
                    "Input dispatch  p50 {:.3} ms  p95 {:.3} ms  p99 {:.3} ms  (n={})",
                    input.dispatch_p50_ms,
                    input.dispatch_p95_ms,
                    input.dispatch_p99_ms,
                    input.dispatch_samples
                ));
            }
            ui.label(if input.active {
                "Forwarding enabled"
            } else {
                "Forwarding inactive"
            });
            ui.separator();
            for slot in &input.slots {
                egui::CollapsingHeader::new(format!("Slot {}: {}", slot.slot, slot.layout))
                    .default_open(slot.slot == 0)
                    .show(ui, |ui| {
                        if !slot.connected {
                            ui.colored_label(egui::Color32::GRAY, "Controller disconnected");
                            return;
                        }
                        ui.strong(&slot.name);
                        ui.monospace(&slot.guid);
                        ui.label(format!(
                            "Sticks L {:?}  R {:?}   Triggers {} / {}   Pointer {:?}",
                            slot.left_stick,
                            slot.right_stick,
                            slot.left_trigger,
                            slot.right_trigger,
                            slot.pointer
                        ));
                        ui.label(if slot.pressed.is_empty() {
                            "Pressed: none".to_owned()
                        } else {
                            format!("Pressed: {}", slot.pressed.join(", "))
                        });
                        ui.label(format!(
                            "Axes: {}",
                            slot.axes
                                .iter()
                                .map(|axis| format!("{}={}", axis.name, axis.value))
                                .collect::<Vec<_>>()
                                .join("  ")
                        ));
                        ui.monospace(format!("Mapped buttons: 0x{:08x}", slot.mapped_buttons));
                        if slot.accelerometer != [0.0; 3] || slot.gyroscope != [0.0; 3] {
                            ui.label(format!(
                                "Sensors: accel {:?} m/s²   gyro {:?} rad/s",
                                slot.accelerometer, slot.gyroscope
                            ));
                        }
                    });
            }
        });
}

pub fn draw_statistics_graph(ui: &mut egui::Ui, history: &VecDeque<StatisticsSample>) {
    const GRAPH_WIDTH: f32 = 420.0;
    const GRAPH_HEIGHT: f32 = 90.0;
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(GRAPH_WIDTH, GRAPH_HEIGHT), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, egui::Color32::from_black_alpha(48));
    for division in 1..4 {
        let y = egui::lerp(rect.top()..=rect.bottom(), division as f32 / 4.0);
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            egui::Stroke::new(1.0, egui::Color32::from_gray(55)),
        );
    }
    if history.len() >= 2 {
        let fps_max = history
            .iter()
            .map(|sample| sample.fps)
            .fold(60.0_f32, f32::max)
            .max(1.0);
        let latency_max = history
            .iter()
            .map(|sample| sample.latency_ms)
            .fold(16.67_f32, f32::max)
            .max(1.0);
        let interval_max = history
            .iter()
            .map(|sample| sample.frame_interval_ms)
            .fold(16.67_f32, f32::max)
            .max(1.0);
        let now = Instant::now();
        let points = |value: fn(&StatisticsSample) -> f32, maximum: f32| {
            history
                .iter()
                .map(|sample| {
                    let age = now.saturating_duration_since(sample.sampled_at);
                    let position =
                        1.0 - age.as_secs_f32() / STATISTICS_ROLLING_WINDOW.as_secs_f32();
                    let x = egui::lerp(rect.left()..=rect.right(), position.clamp(0.0, 1.0));
                    let normalized = (value(sample) / maximum).clamp(0.0, 1.0);
                    egui::pos2(x, egui::lerp(rect.bottom()..=rect.top(), normalized))
                })
                .collect::<Vec<_>>()
        };
        painter.add(egui::Shape::line(
            points(|sample| sample.fps, fps_max),
            egui::Stroke::new(1.5, egui::Color32::LIGHT_GREEN),
        ));
        painter.add(egui::Shape::line(
            points(|sample| sample.latency_ms, latency_max),
            egui::Stroke::new(1.5, egui::Color32::LIGHT_RED),
        ));
        painter.add(egui::Shape::line(
            points(|sample| sample.frame_interval_ms, interval_max),
            egui::Stroke::new(1.5, egui::Color32::LIGHT_BLUE),
        ));
        painter.text(
            rect.left_top() + egui::vec2(5.0, 4.0),
            egui::Align2::LEFT_TOP,
            format!(
                "FPS green {fps_max:.0}   latency red {latency_max:.1} ms   frame Δ blue {interval_max:.1} ms"
            ),
            egui::FontId::monospace(10.0),
            egui::Color32::WHITE,
        );
    } else {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "Collecting frame statistics…",
            egui::FontId::monospace(10.0),
            egui::Color32::GRAY,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{FrameSequenceTracker, PacingRates, PacingTracker, TargetStatisticsTracker};
    use std::time::{Duration, Instant};

    /// The whole point of the pair is comparability, so both rates have to come
    /// from the same window — reading one must not resample the other.
    #[test]
    fn pacing_rates_share_one_sampling_window() {
        let mut tracker = PacingTracker::default();
        let started = Instant::now();
        assert_eq!(tracker.rates_at(started), PacingRates::default());

        // Two composition intervals out of every three carried a present.
        for tick in 0..60 {
            tracker.record_clock();
            if tick % 3 != 0 {
                tracker.record_present();
            }
        }
        let rates = tracker.rates_at(started + Duration::from_secs(1));
        assert_eq!(rates.clock, 60.0);
        assert_eq!(rates.presented, 40.0);

        // A window that has not elapsed reports the last full sample rather
        // than a partial count that would read as a rate collapse.
        tracker.record_clock();
        assert_eq!(
            tracker.rates_at(started + Duration::from_millis(1100)),
            rates
        );
    }

    #[test]
    fn a_new_pacing_window_does_not_carry_the_previous_counts() {
        let mut tracker = PacingTracker::default();
        let started = Instant::now();
        tracker.rates_at(started);
        for _ in 0..120 {
            tracker.record_clock();
            tracker.record_present();
        }
        assert_eq!(
            tracker.rates_at(started + Duration::from_secs(1)).clock,
            120.0
        );
        for _ in 0..60 {
            tracker.record_clock();
        }
        let second = tracker.rates_at(started + Duration::from_secs(2));
        assert_eq!(second.clock, 60.0);
        assert_eq!(second.presented, 0.0, "presents must not carry over");
    }

    #[test]
    fn interleaved_streams_do_not_create_false_drops_or_intervals() {
        let mut tracker = FrameSequenceTracker::default();

        assert_eq!(tracker.observe(1, 1, 100).interval_qpc, None);
        assert_eq!(tracker.observe(2, 100, 105).interval_qpc, None);
        assert_eq!(tracker.observe(1, 2, 116).interval_qpc, Some(16.0));
        assert_eq!(tracker.observe(2, 101, 121).interval_qpc, Some(16.0));
        assert_eq!(tracker.dropped(), 0);
        assert_eq!(tracker.stream_count(), 2);
    }

    #[test]
    fn gaps_are_counted_only_within_the_same_stream() {
        let mut tracker = FrameSequenceTracker::default();

        tracker.observe(1, 10, 100);
        let observation = tracker.observe(1, 13, 148);
        assert_eq!(observation.interval_qpc, Some(16.0));
        assert_eq!(observation.dropped, 2);
        assert_eq!(tracker.dropped(), 2);
    }

    #[test]
    fn a_restarted_sequence_establishes_a_new_baseline() {
        let mut tracker = FrameSequenceTracker::default();

        tracker.observe(1, 100, 1000);
        assert_eq!(tracker.observe(1, 1, 1010).interval_qpc, None);
        assert_eq!(tracker.observe(1, 2, 1026).interval_qpc, Some(16.0));
        assert_eq!(tracker.dropped(), 0);
    }

    #[test]
    fn statistics_are_sampled_independently_per_target() {
        let mut tracker = TargetStatisticsTracker::default();
        let started = Instant::now();
        for frame in 0..60 {
            let now = started + Duration::from_millis(frame * 10);
            tracker.observe_at(10, 0, 2.0, Some(10.0), now);
            if frame % 2 == 0 {
                tracker.observe_at(20, 1, 4.0, Some(20.0), now);
            }
        }

        assert!(tracker.sample_at(started + Duration::from_secs(1)));
        let statistics = tracker.statistics();
        assert_eq!(statistics.len(), 2);
        assert_eq!(statistics[0].target, 0);
        assert_eq!(statistics[0].fps, 60.0);
        assert_eq!(statistics[0].latency_ms, 2.0);
        assert_eq!(statistics[0].frame_interval_ms, 10.0);
        assert_eq!(statistics[1].target, 1);
        assert_eq!(statistics[1].fps, 30.0);
        assert_eq!(statistics[1].latency_ms, 4.0);
        assert_eq!(statistics[1].frame_interval_ms, 20.0);
    }

    #[test]
    fn histories_evict_samples_older_than_120_seconds() {
        let mut tracker = TargetStatisticsTracker::default();
        let started = Instant::now();
        tracker.observe_at(10, 0, 2.0, Some(16.0), started);
        tracker.sample_at(started + Duration::from_secs(1));
        tracker.sample_at(started + Duration::from_secs(122));

        let statistics = tracker.statistics();
        assert_eq!(statistics[0].history.len(), 1);
        assert_eq!(
            statistics[0].history[0].sampled_at,
            started + Duration::from_secs(122)
        );
    }
}
