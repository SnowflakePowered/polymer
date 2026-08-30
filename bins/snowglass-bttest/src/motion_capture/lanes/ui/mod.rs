use eframe::egui;

use super::LaneStage;
use crate::{
    BluetoothTestApp,
    motion_capture::{GAMECUBE_REPORT, PHASES},
    worker::Command,
};

pub(super) enum LaneUiAction {
    None,
    ArmStart,
    ArmEnd,
    Sleep,
    Repeat,
    Abort,
}

impl BluetoothTestApp {
    pub(crate) fn begin_lane_experiment(&mut self) {
        self.begin_lane_experiment_from(false);
    }

    pub(crate) fn begin_sleep_experiment(&mut self) {
        self.begin_lane_experiment_from(true);
    }

    fn begin_lane_experiment_from(&mut self, sleep_only: bool) {
        self.stop_motion_experiment();
        self.class_d_experiment.reset();
        self.timing_experiment.reset();
        self.rare_timing_experiment.reset();
        self.quaternion_experiment.reset();
        let address = self
            .motion_capture
            .as_ref()
            .and_then(|capture| capture.address().parse().ok());
        let error = self.motion_capture.as_mut().and_then(|capture| {
            if sleep_only {
                self.lane_experiment.start_sleep(capture)
            } else {
                self.lane_experiment.start(capture)
            }
            .err()
        });
        if let Some(error) = error {
            self.log(format!("Could not start class-C lane experiment: {error}"));
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

    pub(crate) fn lane_capture_ui(&mut self, ui: &mut egui::Ui) -> bool {
        if !self.lane_experiment.is_engaged() {
            if ui.button("Run guided 30-byte lane experiment…").clicked() {
                self.begin_lane_experiment();
            }
            ui.weak("Native 0x0A only: movements are delimited by actual 30-byte class-C packets, followed by three stationary class-C samples.");
            return self.lane_experiment.is_engaged();
        }

        let stage = self.lane_experiment.stage();
        let action = self.lane_experiment.current_action().copied();
        let native_ready = self.motion_capture.as_ref().is_some_and(|capture| {
            self.connected.contains_key(capture.address())
                && capture.selected_report() == GAMECUBE_REPORT
                && capture.observed_report() == Some(GAMECUBE_REPORT)
        });
        let mut requested = LaneUiAction::None;

        ui.group(|ui| {
            ui.heading("Guided class-C lane capture");
            ui.horizontal(|ui| {
                if let Some(number) = self.lane_experiment.action_number() {
                    ui.label(format!(
                        "Action {number} of {}",
                        self.lane_experiment.action_count()
                    ));
                }
                if let Some((repetition, count)) = self.lane_experiment.repetition() {
                    ui.label(format!("· repetition {repetition} of {count}"));
                }
                ui.label(format!(
                    "· {} class-C boundaries observed",
                    self.lane_experiment.boundaries_observed()
                ));
            });

            if let Some(action) = action {
                ui.strong(action.label);
                match stage {
                    LaneStage::Positioning => {
                        ui.colored_label(
                            egui::Color32::LIGHT_BLUE,
                            action.positioning_instruction,
                        );
                        if !native_ready {
                            ui.colored_label(
                                egui::Color32::YELLOW,
                                "Waiting for native 0x0A notifications…",
                            );
                        }
                        if ui
                            .add_enabled(
                                native_ready,
                                egui::Button::new(
                                    "Ready and still — arm the next class-C start boundary",
                                ),
                            )
                            .clicked()
                        {
                            requested = LaneUiAction::ArmStart;
                        }
                    }
                    LaneStage::AwaitingStartBoundary => {
                        ui.colored_label(
                            egui::Color32::YELLOW,
                            "Keep completely still. Waiting for the next 30-byte class-C packet…",
                        );
                        ui.spinner();
                    }
                    LaneStage::Moving => {
                        ui.colored_label(egui::Color32::LIGHT_GREEN, "GO");
                        ui.label(action.action_instruction);
                        if action.requests_sleep
                            && ui.button("Send bonded sleep command").clicked()
                        {
                            requested = LaneUiAction::Sleep;
                        }
                        if !native_ready {
                            ui.colored_label(
                                egui::Color32::YELLOW,
                                "Native reports are absent. For the sleep trial, press HOME and wait for bonded wake to reconnect.",
                            );
                        }
                        if ui
                            .add_enabled(
                                native_ready,
                                egui::Button::new(
                                    "Action complete — use the next class-C packet as endpoint",
                                ),
                            )
                            .clicked()
                        {
                            requested = LaneUiAction::ArmEnd;
                        }
                    }
                    LaneStage::AwaitingEndBoundary => {
                        ui.colored_label(
                            egui::Color32::YELLOW,
                            "Hold the endpoint pose completely still. Waiting for its class-C boundary…",
                        );
                        ui.spinner();
                    }
                    LaneStage::Holding => {
                        let (captured, total) =
                            self.lane_experiment.hold_progress().unwrap_or_default();
                        ui.colored_label(
                            egui::Color32::LIGHT_BLUE,
                            format!(
                                "Do not move — captured {captured} of {total} stationary follow-up class-C packets"
                            ),
                        );
                        ui.add(
                            egui::ProgressBar::new(f32::from(captured) / f32::from(total))
                                .desired_width(420.0),
                        );
                    }
                    LaneStage::Complete => {}
                    LaneStage::Idle => {}
                }
            }

            if stage == LaneStage::Complete {
                ui.colored_label(
                    egui::Color32::LIGHT_GREEN,
                    "The guided capture is complete. Stop and save it below.",
                );
                let repeat_label = if self.lane_experiment.is_sleep_only() {
                    "Repeat guided sleep/wake sequence"
                } else {
                    "Repeat guided lane sequence"
                };
                if ui.button(repeat_label).clicked() {
                    requested = LaneUiAction::Repeat;
                }
            }
        });

        if self.lane_experiment.is_running()
            && ui.button("Abort lane sequence (keep capture)").clicked()
        {
            requested = LaneUiAction::Abort;
        }
        self.apply_lane_ui_action(requested);
        self.lane_experiment.is_engaged()
    }

    fn apply_lane_ui_action(&mut self, action: LaneUiAction) {
        let error = match action {
            LaneUiAction::None => return,
            LaneUiAction::ArmStart => self
                .motion_capture
                .as_mut()
                .and_then(|capture| self.lane_experiment.arm_start(capture).err()),
            LaneUiAction::ArmEnd => self
                .motion_capture
                .as_mut()
                .and_then(|capture| self.lane_experiment.arm_end(capture).err()),
            LaneUiAction::Sleep => {
                if let Some(addr) = self
                    .motion_capture
                    .as_ref()
                    .and_then(|capture| capture.address().parse().ok())
                {
                    self.send(Command::SleepBle(addr));
                }
                None
            }
            LaneUiAction::Repeat => self
                .motion_capture
                .as_mut()
                .and_then(|capture| self.lane_experiment.restart(capture).err()),
            LaneUiAction::Abort => {
                let freeform_label = PHASES[self.motion_capture_phase].label;
                self.motion_capture.as_mut().and_then(|capture| {
                    self.lane_experiment
                        .abort(capture)
                        .and_then(|()| capture.set_label(freeform_label))
                        .err()
                })
            }
        };
        if let Some(error) = error {
            self.log(format!("Could not update class-C lane experiment: {error}"));
            self.finish_motion_capture(false);
        }
    }
}
