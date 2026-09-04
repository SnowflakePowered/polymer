use eframe::egui;

use super::ClassDStage;
use crate::{
    BluetoothTestApp,
    motion_capture::{GAMECUBE_REPORT, PHASES},
    worker::Command,
};

enum ClassDUiAction {
    None,
    Begin,
    Repeat,
    Abort,
}

impl BluetoothTestApp {
    pub(crate) fn begin_class_d_experiment(&mut self) {
        self.stop_motion_experiment();
        self.lane_experiment.reset();
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
            .and_then(|capture| self.class_d_experiment.start(capture).err());
        if let Some(error) = error {
            self.log(format!("Could not start class-D experiment: {error}"));
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

    pub(crate) fn class_d_capture_ui(&mut self, ui: &mut egui::Ui) -> bool {
        if !self.class_d_experiment.is_engaged() {
            if ui.button("Run guided class-D experiment…").clicked() {
                self.begin_class_d_experiment();
            }
            ui.weak("Native 0x0A only: each manipulation continues until eight actual timing-class 0xD packets have been captured.");
            return self.class_d_experiment.is_engaged();
        }

        let stage = self.class_d_experiment.stage();
        let action = self.class_d_experiment.current_action().copied();
        let native_ready = self.motion_capture.as_ref().is_some_and(|capture| {
            self.connected.contains_key(capture.address())
                && capture.selected_report() == GAMECUBE_REPORT
                && capture.observed_report() == Some(GAMECUBE_REPORT)
        });
        let mut requested = ClassDUiAction::None;

        ui.group(|ui| {
            ui.heading("Guided class-D capture");
            ui.horizontal(|ui| {
                if let Some(number) = self.class_d_experiment.action_number() {
                    ui.label(format!(
                        "Action {number} of {}",
                        self.class_d_experiment.action_count()
                    ));
                }
                ui.label(format!(
                    "· {} total class-D packets",
                    self.class_d_experiment.total_observed()
                ));
            });

            if let Some(action) = action {
                ui.strong(action.label);
                match stage {
                    ClassDStage::Positioning => {
                        ui.colored_label(
                            egui::Color32::LIGHT_BLUE,
                            action.positioning_instruction,
                        );
                        ui.label("Do not perform the manipulation yet. Press Start when positioned; movement happens during the capture step.");
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
                            requested = ClassDUiAction::Begin;
                        }
                    }
                    ClassDStage::Capturing => {
                        ui.colored_label(egui::Color32::LIGHT_GREEN, "CAPTURING — perform the manipulation now");
                        ui.label(action.capture_instruction);
                        let observed = self.class_d_experiment.observed_in_action();
                        let target = self.class_d_experiment.target_per_action();
                        ui.strong(format!(
                            "Captured {observed} of {target} class-D packets for this action"
                        ));
                        ui.add(
                            egui::ProgressBar::new(f32::from(observed) / f32::from(target))
                                .desired_width(420.0),
                        );
                        ui.weak("Class-D packets are naturally infrequent. Keep repeating the indicated movement until this advances automatically.");
                    }
                    ClassDStage::Complete | ClassDStage::Idle => {}
                }
            }

            if stage == ClassDStage::Complete {
                ui.colored_label(
                    egui::Color32::LIGHT_GREEN,
                    "All class-D actions are complete. Stop and save the capture below.",
                );
                if ui.button("Repeat guided class-D sequence").clicked() {
                    requested = ClassDUiAction::Repeat;
                }
            }
        });

        if self.class_d_experiment.is_running()
            && ui.button("Abort class-D sequence (keep capture)").clicked()
        {
            requested = ClassDUiAction::Abort;
        }
        self.apply_class_d_ui_action(requested);
        self.class_d_experiment.is_engaged()
    }

    fn apply_class_d_ui_action(&mut self, action: ClassDUiAction) {
        let error = match action {
            ClassDUiAction::None => return,
            ClassDUiAction::Begin => self
                .motion_capture
                .as_mut()
                .and_then(|capture| self.class_d_experiment.begin_action(capture).err()),
            ClassDUiAction::Repeat => self
                .motion_capture
                .as_mut()
                .and_then(|capture| self.class_d_experiment.restart(capture).err()),
            ClassDUiAction::Abort => {
                let freeform_label = PHASES[self.motion_capture_phase].label;
                self.motion_capture.as_mut().and_then(|capture| {
                    self.class_d_experiment
                        .abort(capture)
                        .and_then(|()| capture.set_label(freeform_label))
                        .err()
                })
            }
        };
        if let Some(error) = error {
            self.log(format!("Could not update class-D experiment: {error}"));
            self.finish_motion_capture(false);
        }
    }
}
