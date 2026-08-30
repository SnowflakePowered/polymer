use eframe::egui;

use super::TimingStage;
use crate::{
    BluetoothTestApp,
    motion_capture::{GAMECUBE_REPORT, PHASES},
    worker::Command,
};

enum TimingUiAction {
    None,
    Begin,
    Skip,
    Repeat,
    Abort,
}

impl BluetoothTestApp {
    pub(crate) fn begin_timing_experiment(&mut self) {
        self.stop_motion_experiment();
        self.lane_experiment.reset();
        self.class_d_experiment.reset();
        self.rare_timing_experiment.reset();
        self.quaternion_experiment.reset();
        let address = self
            .motion_capture
            .as_ref()
            .and_then(|capture| capture.address().parse().ok());
        let error = self
            .motion_capture
            .as_mut()
            .and_then(|capture| self.timing_experiment.start(capture).err());
        if let Some(error) = error {
            self.log(format!("Could not start timing experiment: {error}"));
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

    pub(crate) fn timing_capture_ui(&mut self, ui: &mut egui::Ui) -> bool {
        if !self.timing_experiment.is_engaged() {
            if ui.button("Run bit 287 + timing experiment…").clicked() {
                self.begin_timing_experiment();
            }
            ui.weak("Native 0x0A only: captures real class-F packets with their preceding/following packet and both host and sensor-clock intervals.");
            return self.timing_experiment.is_engaged();
        }

        let stage = self.timing_experiment.stage();
        let action = self.timing_experiment.current_action().copied();
        let native_ready = self.motion_capture.as_ref().is_some_and(|capture| {
            self.connected.contains_key(capture.address())
                && capture.selected_report() == GAMECUBE_REPORT
                && capture.observed_report() == Some(GAMECUBE_REPORT)
        });
        let mut requested = TimingUiAction::None;

        ui.group(|ui| {
            ui.heading("Bit 287 + packed timing capture");
            ui.horizontal(|ui| {
                if let Some(number) = self.timing_experiment.action_number() {
                    ui.label(format!(
                        "Action {number} of {}",
                        self.timing_experiment.action_count()
                    ));
                }
                ui.label(format!(
                    "· {} class-F contexts",
                    self.timing_experiment.total_observed()
                ));
            });
            let bit_counts = self.timing_experiment.bit_287_counts();
            ui.monospace(format!(
                "class-F bit 287: zero {} · one {}",
                bit_counts[0], bit_counts[1]
            ));
            ui.monospace(format!(
                "class-E elapsed outside 11–13 ticks: {}",
                self.timing_experiment.elapsed_outliers()
            ));

            if let Some(action) = action {
                ui.strong(action.label);
                match stage {
                    TimingStage::Positioning => {
                        ui.colored_label(
                            egui::Color32::LIGHT_BLUE,
                            action.positioning_instruction,
                        );
                        ui.label("Do not perform the manipulation until capture starts.");
                        if !native_ready {
                            ui.colored_label(
                                egui::Color32::YELLOW,
                                "Waiting for native 0x0A notifications…",
                            );
                        }
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(native_ready, egui::Button::new("Start this action"))
                                .clicked()
                            {
                                requested = TimingUiAction::Begin;
                            }
                            if action.optional && ui.button("Skip optional action").clicked() {
                                requested = TimingUiAction::Skip;
                            }
                        });
                    }
                    TimingStage::Capturing => {
                        ui.colored_label(egui::Color32::LIGHT_GREEN, "CAPTURING");
                        ui.label(action.capture_instruction);
                        let observed = self.timing_experiment.observed_in_action();
                        let target = self.timing_experiment.target_per_action();
                        ui.strong(format!(
                            "Captured {observed} of {target} complete class-F contexts"
                        ));
                        ui.add(
                            egui::ProgressBar::new(f32::from(observed) / f32::from(target))
                                .desired_width(420.0),
                        );
                        ui.weak("Class-F packets are naturally infrequent. The action advances only after each packet's following context packet arrives.");
                    }
                    TimingStage::Complete | TimingStage::Idle => {}
                }
            }

            if stage == TimingStage::Complete {
                ui.colored_label(
                    egui::Color32::LIGHT_GREEN,
                    "All timing actions are complete. Stop and save the capture below.",
                );
                if ui.button("Repeat timing sequence").clicked() {
                    requested = TimingUiAction::Repeat;
                }
            }
        });

        if self.timing_experiment.is_running()
            && ui.button("Abort timing sequence (keep capture)").clicked()
        {
            requested = TimingUiAction::Abort;
        }
        self.apply_timing_ui_action(requested);
        self.timing_experiment.is_engaged()
    }

    fn apply_timing_ui_action(&mut self, action: TimingUiAction) {
        let error = match action {
            TimingUiAction::None => return,
            TimingUiAction::Begin => self
                .motion_capture
                .as_mut()
                .and_then(|capture| self.timing_experiment.begin_action(capture).err()),
            TimingUiAction::Skip => self
                .motion_capture
                .as_mut()
                .and_then(|capture| self.timing_experiment.skip_action(capture).err()),
            TimingUiAction::Repeat => self
                .motion_capture
                .as_mut()
                .and_then(|capture| self.timing_experiment.restart(capture).err()),
            TimingUiAction::Abort => {
                let freeform_label = PHASES[self.motion_capture_phase].label;
                self.motion_capture.as_mut().and_then(|capture| {
                    self.timing_experiment
                        .abort(capture)
                        .and_then(|()| capture.set_label(freeform_label))
                        .err()
                })
            }
        };
        if let Some(error) = error {
            self.log(format!("Could not update timing experiment: {error}"));
            self.finish_motion_capture(false);
        }
    }
}
