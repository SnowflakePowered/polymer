use eframe::egui;

use super::{MotionAxis, RareTimingStage, TARGET_CONTEXTS_PER_AXIS, TARGET_ELAPSED_TICKS};
use crate::{
    BluetoothTestApp,
    motion_capture::{GAMECUBE_REPORT, PHASES},
    worker::Command,
};

enum RareTimingUiAction {
    None,
    Begin,
    Position(MotionAxis),
    ReselectNative,
    Repeat,
    Abort,
}

impl BluetoothTestApp {
    pub(crate) fn begin_rare_timing_experiment(&mut self) {
        self.stop_motion_experiment();
        self.lane_experiment.reset();
        self.class_d_experiment.reset();
        self.timing_experiment.reset();
        self.quaternion_experiment.reset();
        let address = self
            .motion_capture
            .as_ref()
            .and_then(|capture| capture.address().parse().ok());
        let error = self
            .motion_capture
            .as_mut()
            .and_then(|capture| self.rare_timing_experiment.start(capture).err());
        if let Some(error) = error {
            self.log(format!("Could not start rare timing experiment: {error}"));
            self.finish_motion_capture(false);
            return;
        }
        if let Some(addr) = address {
            self.send(Command::SelectSwitch2InputReport {
                addr,
                report_id: GAMECUBE_REPORT,
            });
        }
    }

    pub(crate) fn rare_timing_capture_ui(&mut self, ui: &mut egui::Ui) -> bool {
        if !self.rare_timing_experiment.is_engaged() {
            return false;
        }

        let stage = self.rare_timing_experiment.stage();
        let axis = self.rare_timing_experiment.current_axis();
        let native_ready = self.motion_capture.as_ref().is_some_and(|capture| {
            self.connected.contains_key(capture.address())
                && capture.selected_report() == GAMECUBE_REPORT
                && capture.observed_report() == Some(GAMECUBE_REPORT)
        });
        let mut requested = RareTimingUiAction::None;

        ui.group(|ui| {
            ui.heading("Rare class-F sample timing capture");
            ui.label("Collects the nearest two normal class-E anchors on each side of every targeted class-F packet, preserving every packet between them.");
            ui.weak("Elapsed-15 contexts also require and record the observed full D9 continuation packet.");
            ui.horizontal_wrapped(|ui| {
                for candidate in MotionAxis::ALL {
                    let selected = axis == Some(candidate);
                    if ui
                        .selectable_label(selected, format!("{} axis", candidate.label()))
                        .clicked()
                        && !selected
                    {
                        requested = RareTimingUiAction::Position(candidate);
                    }
                }
            });

            self.rare_timing_quota_ui(ui);

            ui.horizontal(|ui| {
                ui.label(format!(
                    "{} complete contexts",
                    self.rare_timing_experiment.completed_contexts()
                ));
                ui.label(format!(
                    "· {} rejected (missing anchors, stale window, or invalid F15 continuation)",
                    self.rare_timing_experiment.rejected_contexts()
                ));
            });
            let target = self.rare_timing_experiment.target_contexts();
            let progress = self
                .rare_timing_experiment
                .completed_by_axis()
                .iter()
                .flatten()
                .map(|count| u64::from((*count).min(TARGET_CONTEXTS_PER_AXIS)))
                .sum::<u64>();
            ui.add(
                egui::ProgressBar::new(progress as f32 / target as f32)
                    .text(format!("quota {progress}/{target}"))
                    .desired_width(520.0),
            );

            match (stage, axis) {
                (RareTimingStage::Positioning, Some(axis)) => {
                    ui.colored_label(egui::Color32::LIGHT_BLUE, axis.instruction());
                    ui.label("Position the controller first. Begin moving only after capture starts.");
                    if !native_ready {
                        ui.colored_label(
                            egui::Color32::YELLOW,
                            "Waiting for native 0x0A notifications…",
                        );
                    }
                    if ui
                        .add_enabled(
                            native_ready,
                            egui::Button::new(format!("Start {} capture", axis.label())),
                        )
                        .clicked()
                    {
                        requested = RareTimingUiAction::Begin;
                    }
                }
                (RareTimingStage::Capturing, Some(axis)) => {
                    ui.colored_label(egui::Color32::LIGHT_GREEN, "CAPTURING");
                    ui.strong(axis.instruction());
                    ui.label("Do not reverse direction while CAPTURING. Pause and reposition before moving back for the next arc.");
                    ui.weak("The unresolved priority is elapsed 47-48. Overlapping candidates and every intervening C/D/F packet are retained.");
                    ui.weak("To encourage 35–48 tick spans, increase distance or partially shield the controller from the BLE adapter. Do not stall the application.");
                    if ui
                        .button("Re-select native 0x0A (probe timing transition)")
                        .clicked()
                    {
                        requested = RareTimingUiAction::ReselectNative;
                    }
                    ui.weak("Use the re-selection probe occasionally while continuing the same smooth motion; it may expose the elapsed-15 transition case.");
                    if ui.button("Pause and reposition").clicked() {
                        requested = RareTimingUiAction::Position(axis);
                    }
                }
                (RareTimingStage::Complete, _) => {
                    ui.colored_label(
                        egui::Color32::LIGHT_GREEN,
                        "All elapsed-span and motion-axis quotas are complete.",
                    );
                    if ui.button("Repeat rare timing experiment").clicked() {
                        requested = RareTimingUiAction::Repeat;
                    }
                }
                (RareTimingStage::Idle, _)
                | (RareTimingStage::Positioning | RareTimingStage::Capturing, None) => {}
            }
        });

        if self.rare_timing_experiment.is_running()
            && ui
                .button("Abort rare timing experiment (keep capture)")
                .clicked()
        {
            requested = RareTimingUiAction::Abort;
        }
        self.apply_rare_timing_ui_action(requested);
        self.rare_timing_experiment.is_engaged()
    }

    fn rare_timing_quota_ui(&self, ui: &mut egui::Ui) {
        let completed = self.rare_timing_experiment.completed_by_axis();
        let observed = self.rare_timing_experiment.observed_candidates();
        egui::Grid::new("rare_timing_quota_grid")
            .striped(true)
            .show(ui, |ui| {
                ui.strong("Elapsed");
                for axis in MotionAxis::ALL {
                    ui.strong(axis.label());
                }
                ui.strong("Candidates");
                ui.end_row();

                for (bucket, elapsed) in TARGET_ELAPSED_TICKS.iter().enumerate() {
                    ui.monospace(format!("{elapsed} ticks"));
                    for axis in MotionAxis::ALL {
                        let count = completed[axis.index()][bucket];
                        let text = format!("{count}/{TARGET_CONTEXTS_PER_AXIS}");
                        if count >= TARGET_CONTEXTS_PER_AXIS {
                            ui.colored_label(egui::Color32::LIGHT_GREEN, text);
                        } else {
                            ui.monospace(text);
                        }
                    }
                    let candidates = MotionAxis::ALL
                        .iter()
                        .map(|axis| observed[axis.index()][bucket])
                        .sum::<u64>();
                    ui.monospace(candidates.to_string());
                    ui.end_row();
                }
            });
    }

    fn apply_rare_timing_ui_action(&mut self, action: RareTimingUiAction) {
        if matches!(action, RareTimingUiAction::ReselectNative) {
            if let Some(addr) = self
                .motion_capture
                .as_ref()
                .and_then(|capture| capture.address().parse().ok())
            {
                self.send(Command::SelectSwitch2InputReport {
                    addr,
                    report_id: GAMECUBE_REPORT,
                });
            }
            return;
        }
        let error = match action {
            RareTimingUiAction::None => return,
            RareTimingUiAction::Begin => self
                .motion_capture
                .as_mut()
                .and_then(|capture| self.rare_timing_experiment.begin_axis(capture).err()),
            RareTimingUiAction::Position(axis) => {
                self.motion_capture.as_mut().and_then(|capture| {
                    self.rare_timing_experiment
                        .position_axis(capture, axis)
                        .err()
                })
            }
            RareTimingUiAction::Repeat => self
                .motion_capture
                .as_mut()
                .and_then(|capture| self.rare_timing_experiment.restart(capture).err()),
            RareTimingUiAction::Abort => {
                let freeform_label = PHASES[self.motion_capture_phase].label;
                self.motion_capture.as_mut().and_then(|capture| {
                    self.rare_timing_experiment
                        .abort(capture)
                        .and_then(|()| capture.set_label(freeform_label))
                        .err()
                })
            }
            RareTimingUiAction::ReselectNative => unreachable!(),
        };
        if let Some(error) = error {
            self.log(format!("Could not update rare timing experiment: {error}"));
            self.finish_motion_capture(false);
        }
    }
}
