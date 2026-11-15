use std::{error::Error, time::Instant};

use polymer_statistics::{ROLLING_WINDOW, StatisticsSample, StatisticsSnapshot};

/// Records egui output into a backend's compositor frame.
pub trait EguiPainter {
    type Compositor;
    type RenderPass<'frame>;

    /// Applies updates that must happen before the compositor opens its render pass.
    /// Call this before `present_with`, then call `paint` inside its callback.
    fn prepare(
        &mut self,
        _compositor: &Self::Compositor,
        _output: &egui::FullOutput,
    ) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    /// Uploads any remaining texture changes and records the tessellated primitives.
    fn paint(
        &mut self,
        pass: &mut Self::RenderPass<'_>,
        output: &egui::FullOutput,
        primitives: &[egui::ClippedPrimitive],
    ) -> Result<(), Box<dyn Error>>;
}

#[derive(Clone, Copy, Debug)]
pub struct RenderTargetExtent {
    pub connection: u64,
    pub target: u32,
    pub extent: crate::Extent,
}

#[derive(Clone, Debug, Default)]
pub struct DebugOverlayModel {
    pub backend: String,
    pub consumer: String,
    pub notes: Vec<String>,
    pub frame_number: u64,
    pub slot: u32,
    pub render_targets: Vec<RenderTargetExtent>,
    pub statistics: StatisticsSnapshot,
    pub input: InputDiagnostics,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct InputAxisDiagnostics {
    pub name: String,
    pub value: i16,
}

#[derive(Clone, Debug, Default, PartialEq)]
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

#[derive(Clone, Debug, Default, PartialEq)]
pub struct InputDiagnostics {
    pub active: bool,
    pub slots: Vec<InputSlotDiagnostics>,
}

pub fn draw_debug_overlay(context: &egui::Context, model: &DebugOverlayModel) {
    egui::TopBottomPanel::top("polymer-debug-statistics")
        .exact_height(32.0)
        .frame(egui::Frame::side_top_panel(&context.style()).fill(egui::Color32::TRANSPARENT))
        .show(context, |ui| {
            egui::ScrollArea::horizontal().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.strong(format!("Polymer {} via {}", model.backend, model.consumer));
                    ui.label(format!("Frame {}", model.frame_number));
                    ui.label(format!("Slot {}", model.slot));
                    for render_target in &model.render_targets {
                        ui.label(format!(
                            "Render target {}: {}x{}",
                            render_target.target,
                            render_target.extent.width,
                            render_target.extent.height
                        ));
                    }
                    for target in &model.statistics.targets {
                        ui.label(format!(
                            "Target {}: {:.1} FPS · latency {:.2} ms · frame Δ {:.2} ms",
                            target.target,
                            target.fps,
                            target.capture_latency.average,
                            target.frame_interval.average,
                        ));
                    }
                    ui.label(format!("Dropped {}", model.statistics.dropped_frames));
                    ui.label(format!(
                        "Presented {:.1}/s · clock {:.1}/s",
                        model.statistics.pacing.presented, model.statistics.pacing.clock
                    ));
                    if let Some(latency) = model.statistics.display_latency.values().next() {
                        ui.colored_label(
                            egui::Color32::LIGHT_BLUE,
                            format!(
                                "To display {:.1} ms · worst {:.1} ms",
                                latency.average, latency.maximum
                            ),
                        );
                    }
                    for note in &model.notes {
                        ui.label(note);
                    }
                });
            });
        });
    let frame = egui::Frame::window(&context.style())
        .fill(egui::Color32::TRANSPARENT)
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_white_alpha(48)))
        .shadow(egui::epaint::Shadow::NONE);
    egui::Window::new("Frame Graphs · rolling 120 seconds")
        .default_pos(egui::pos2(12.0, 48.0))
        .resizable(false)
        .collapsible(true)
        .frame(frame)
        .show(context, |ui| {
            if model.statistics.targets.is_empty() {
                ui.label("Collecting per-target frame statistics…");
            }
            for target in &model.statistics.targets {
                ui.strong(format!("Target {}", target.target));
                draw_statistics_graph(ui, &target.history);
            }
        });
    draw_input_debug_window(context, &model.input, model.statistics.input.combined());
}

pub fn draw_input_debug_window(
    context: &egui::Context,
    input: &InputDiagnostics,
    dispatch: polymer_statistics::DistributionStatistics,
) {
    egui::Window::new("Input Debug")
        .default_pos(egui::pos2(880.0, 48.0))
        .default_width(380.0)
        .show(context, |ui| {
            if dispatch.samples == 0 {
                ui.colored_label(egui::Color32::GRAY, "Input dispatch latency: waiting");
            } else {
                ui.label(format!(
                    "Input dispatch  p50 {:.3} ms  p95 {:.3} ms  p99 {:.3} ms  (n={})",
                    dispatch.p50, dispatch.p95, dispatch.p99, dispatch.samples
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

pub fn draw_statistics_graph(
    ui: &mut egui::Ui,
    history: &std::collections::VecDeque<StatisticsSample>,
) {
    const WIDTH: f32 = 420.0;
    const HEIGHT: f32 = 90.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(WIDTH, HEIGHT), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, egui::Color32::from_black_alpha(48));
    for division in 1..4 {
        let y = egui::lerp(rect.top()..=rect.bottom(), division as f32 / 4.0);
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            egui::Stroke::new(1.0, egui::Color32::from_gray(55)),
        );
    }
    if history.len() < 2 {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "Collecting frame statistics…",
            egui::FontId::monospace(10.0),
            egui::Color32::GRAY,
        );
        return;
    }
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
                let position = 1.0 - age.as_secs_f32() / ROLLING_WINDOW.as_secs_f32();
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
}
