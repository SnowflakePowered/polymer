use eframe::egui;

use super::AccelerationStage;
use crate::{
    BluetoothTestApp,
    motion_capture::{GAMECUBE_REPORT, PHASES},
    worker::Command,
};

enum AccelerationUiAction {
    None,
    Begin,
    Pause,
    Select(usize),
    Repeat,
    Abort,
}

impl BluetoothTestApp {
    pub(crate) fn begin_class_c_acceleration_experiment(&mut self) {
        self.stop_motion_experiment();
        self.lane_experiment.reset();
        self.class_d_experiment.reset();
        self.timing_experiment.reset();
        self.rare_timing_experiment.reset();
        self.quaternion_experiment.reset();
        let address = self
            .motion_capture
            .as_ref()
            .and_then(|capture| capture.address().parse().ok());
        let error = self
            .motion_capture
            .as_mut()
            .and_then(|capture| self.class_c_acceleration_experiment.start(capture).err());
        if let Some(error) = error {
            self.log(format!(
                "Could not start class-C acceleration experiment: {error}"
            ));
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

    pub(crate) fn class_c_acceleration_capture_ui(&mut self, ui: &mut egui::Ui) -> bool {
        if !self.class_c_acceleration_experiment.is_engaged() {
            return false;
        }

        let stage = self.class_c_acceleration_experiment.stage();
        let action = self
            .class_c_acceleration_experiment
            .current_action()
            .copied();
        let native_ready = self.motion_capture.as_ref().is_some_and(|capture| {
            self.connected.contains_key(capture.address())
                && capture.selected_report() == GAMECUBE_REPORT
                && capture.observed_report() == Some(GAMECUBE_REPORT)
        });
        let mut requested = AccelerationUiAction::None;

        ui.group(|ui| {
            ui.heading("Class-C acceleration filter capture");
            ui.label("Separates a fractional sample time from a frequency-dependent acceleration filter using fixed-orientation translation.");
            ui.horizontal(|ui| {
                if let Some(number) = self.class_c_acceleration_experiment.action_number() {
                    ui.label(format!(
                        "Action {number} of {}",
                        self.class_c_acceleration_experiment.action_count()
                    ));
                }
                ui.label(format!(
                    "· {} complete contexts",
                    self.class_c_acceleration_experiment.total_observed()
                ));
                ui.label(format!(
                    "· {} rejected",
                    self.class_c_acceleration_experiment.rejected()
                ));
            });

            let selected_action = self
                .class_c_acceleration_experiment
                .action_number()
                .map(|number| number - 1);
            let selected_title = selected_action
                .and_then(|index| {
                    self.class_c_acceleration_experiment
                        .actions()
                        .get(index)
                })
                .map_or("Choose a trial", |action| action.title);
            egui::ComboBox::from_label("Trial")
                .selected_text(selected_title)
                .show_ui(ui, |ui| {
                    for (index, candidate) in self
                        .class_c_acceleration_experiment
                        .actions()
                        .iter()
                        .enumerate()
                    {
                        let completed = self
                            .class_c_acceleration_experiment
                            .completed_for_action(index);
                        let text = format!(
                            "{} — {completed}/{}",
                            candidate.title, candidate.target
                        );
                        if ui
                            .selectable_label(selected_action == Some(index), text)
                            .clicked()
                            && selected_action != Some(index)
                        {
                            requested = AccelerationUiAction::Select(index);
                        }
                    }
            });
            ui.weak("Selecting another trial pauses the current one and preserves every completed context.");
            ui.colored_label(
                egui::Color32::YELLOW,
                "Current priority: X translation · fast, then X isolated impulses. A medium Y trial is the next useful cross-axis check.",
            );

            if let Some(action) = action {
                ui.strong(action.title);
                match stage {
                    AccelerationStage::Positioning => {
                        ui.colored_label(
                            egui::Color32::LIGHT_BLUE,
                            action.positioning_instruction,
                        );
                        ui.label("Do not begin moving until capture starts. Translation matters; avoid intentional pitch, roll, or yaw.");
                        if !native_ready {
                            ui.colored_label(
                                egui::Color32::YELLOW,
                                "Waiting for native 0x0A notifications…",
                            );
                        }
                        if ui
                            .add_enabled(native_ready, egui::Button::new("Start this action"))
                            .clicked()
                        {
                            requested = AccelerationUiAction::Begin;
                        }
                    }
                    AccelerationStage::Capturing => {
                        ui.colored_label(egui::Color32::LIGHT_GREEN, "CAPTURING");
                        ui.strong(action.capture_instruction);
                        let observed = self.class_c_acceleration_experiment.observed_in_action();
                        ui.label(format!(
                            "Captured {observed} of {} complete E → C → E contexts",
                            action.target
                        ));
                        ui.add(
                            egui::ProgressBar::new(f32::from(observed) / f32::from(action.target))
                                .desired_width(460.0),
                        );
                        ui.weak("Class-C packets are infrequent. Keep the requested motion continuous until the action advances automatically.");
                        if ui.button("Pause and reposition").clicked() {
                            requested = AccelerationUiAction::Pause;
                        }
                    }
                    AccelerationStage::Idle | AccelerationStage::Complete => {}
                }
            }

            if stage == AccelerationStage::Complete {
                ui.colored_label(
                    egui::Color32::LIGHT_GREEN,
                    "All acceleration rates and impulse actions are complete. Stop and save the capture below.",
                );
                if ui.button("Repeat acceleration sequence").clicked() {
                    requested = AccelerationUiAction::Repeat;
                }
            }
        });

        if self.class_c_acceleration_experiment.is_running()
            && ui
                .button("Abort class-C acceleration sequence (keep capture)")
                .clicked()
        {
            requested = AccelerationUiAction::Abort;
        }
        self.apply_class_c_acceleration_ui_action(requested);
        self.class_c_acceleration_experiment.is_engaged()
    }

    fn apply_class_c_acceleration_ui_action(&mut self, action: AccelerationUiAction) {
        let error = match action {
            AccelerationUiAction::None => return,
            AccelerationUiAction::Begin => self.motion_capture.as_mut().and_then(|capture| {
                self.class_c_acceleration_experiment
                    .begin_action(capture)
                    .err()
            }),
            AccelerationUiAction::Pause => self.motion_capture.as_mut().and_then(|capture| {
                self.class_c_acceleration_experiment
                    .pause_action(capture)
                    .err()
            }),
            AccelerationUiAction::Select(action) => {
                self.motion_capture.as_mut().and_then(|capture| {
                    self.class_c_acceleration_experiment
                        .select_action(capture, action)
                        .err()
                })
            }
            AccelerationUiAction::Repeat => self
                .motion_capture
                .as_mut()
                .and_then(|capture| self.class_c_acceleration_experiment.restart(capture).err()),
            AccelerationUiAction::Abort => {
                let freeform_label = PHASES[self.motion_capture_phase].label;
                self.motion_capture.as_mut().and_then(|capture| {
                    self.class_c_acceleration_experiment
                        .abort(capture)
                        .and_then(|()| capture.set_label(freeform_label))
                        .err()
                })
            }
        };
        if let Some(error) = error {
            self.log(format!(
                "Could not update class-C acceleration experiment: {error}"
            ));
            self.finish_motion_capture(false);
        }
    }
}
