//! Reusable egui diagnostics shared by native Snowglass hosts.

use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, Default)]
pub struct StatisticsSample {
    pub fps: f32,
    pub latency_ms: f32,
    pub frame_interval_ms: f32,
}

#[derive(Clone, Debug, Default)]
pub struct FrameDiagnostics {
    pub backend: String,
    pub consumer: String,
    pub frame_number: u64,
    pub slot: u32,
    pub render_width: u32,
    pub render_height: u32,
    pub capture_x: u32,
    pub capture_y: u32,
    pub capture_width: u32,
    pub capture_height: u32,
    pub frames_per_second: f64,
    pub latency_ms: f64,
    pub frame_interval_ms: f64,
    pub dropped: u64,
    pub history: VecDeque<StatisticsSample>,
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
            ui.label(format!("{:.1} FPS", frame.frames_per_second));
            ui.label(format!("Latency {:.2} ms", frame.latency_ms));
            ui.label(format!("Frame-to-frame {:.2} ms", frame.frame_interval_ms));
            ui.label(format!("Dropped {}", frame.dropped));
        });
    });

    let transparent_frame = egui::Frame::window(&context.style())
        .fill(egui::Color32::TRANSPARENT)
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_white_alpha(48)))
        .shadow(egui::epaint::Shadow::NONE);
    egui::Window::new("Frame Graph")
        .default_pos(egui::pos2(12.0, 48.0))
        .resizable(false)
        .collapsible(true)
        .frame(transparent_frame)
        .show(context, |ui| {
            draw_statistics_graph(ui, &diagnostics.frame.history);
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
        let points = |value: fn(&StatisticsSample) -> f32, maximum: f32| {
            history
                .iter()
                .enumerate()
                .map(|(index, sample)| {
                    let x = egui::lerp(
                        rect.left()..=rect.right(),
                        index as f32 / (history.len() - 1) as f32,
                    );
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
