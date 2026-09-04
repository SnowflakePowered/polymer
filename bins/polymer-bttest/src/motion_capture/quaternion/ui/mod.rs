use std::time::Instant;

use eframe::egui;

use super::{HOLD_DURATION, QuaternionStage};
use crate::{
    BluetoothTestApp,
    motion_capture::{GAMECUBE_REPORT, MotionCapture, PHASES},
    worker::Command,
};

impl BluetoothTestApp {
    pub(crate) fn start_motion_capture(&mut self, address: &str, guided: bool) {
        match MotionCapture::start(address) {
            Ok(mut capture) => {
                let initialization = if guided {
                    self.quaternion_experiment.start(&mut capture)
                } else {
                    capture.set_label(PHASES[self.motion_capture_phase].label)
                };
                if let Err(error) = initialization {
                    self.log(format!("Could not initialize motion capture: {error}"));
                    return;
                }

                let path = capture.path().display().to_string();
                self.motion_capture = Some(capture);
                if let Ok(addr) = address.parse() {
                    self.send(Command::SelectSwitch2InputReport {
                        addr,
                        report_id: GAMECUBE_REPORT,
                    });
                }
                let kind = if guided { "Guided motion" } else { "Motion" };
                self.log(format!("{kind} capture started: {path}"));
            }
            Err(error) => self.log(format!("Could not start motion capture: {error}")),
        }
    }

    pub(crate) fn begin_quaternion_experiment(&mut self) {
        self.stop_motion_experiment();
        self.lane_experiment.reset();
        self.class_d_experiment.reset();
        self.timing_experiment.reset();
        self.rare_timing_experiment.reset();
        let address = self
            .motion_capture
            .as_ref()
            .and_then(|capture| capture.address().parse().ok());
        let error = self
            .motion_capture
            .as_mut()
            .and_then(|capture| self.quaternion_experiment.start(capture).err());
        if let Some(error) = error {
            self.log(format!("Could not start quaternion experiment: {error}"));
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

    pub(crate) fn tick_quaternion_experiment(&mut self) {
        let Some(capture) = self.motion_capture.as_mut() else {
            self.quaternion_experiment.reset();
            return;
        };
        if let Err(error) = self.quaternion_experiment.tick(capture, Instant::now()) {
            self.log(format!("Could not advance quaternion experiment: {error}"));
            self.finish_motion_capture(false);
        }
    }

    pub(crate) fn quaternion_capture_ui(&mut self, ui: &mut egui::Ui) -> bool {
        if !self.quaternion_experiment.is_engaged() {
            if ui.button("Run guided quaternion experiment…").clicked() {
                self.begin_quaternion_experiment();
            }
            ui.weak("Native 0x0A only: a guided reference plus quarter-turn holds in both directions for yaw, pitch, and roll.");
            return self.quaternion_experiment.is_engaged();
        }

        let now = Instant::now();
        let stage = self.quaternion_experiment.stage();
        let native_ready = self.motion_capture.as_ref().is_some_and(|capture| {
            capture.selected_report() == GAMECUBE_REPORT
                && capture.observed_report() == Some(GAMECUBE_REPORT)
        });
        let step_number = self.quaternion_experiment.step_number();
        let step_count = self.quaternion_experiment.step_count();
        let step = self.quaternion_experiment.current_step().copied();

        ui.group(|ui| {
            ui.heading("Guided quaternion capture");
            if let Some(number) = step_number {
                ui.label(format!("Step {number} of {step_count}"));
            }
            if let Some(step) = step {
                ui.colored_label(egui::Color32::LIGHT_BLUE, step.instruction);
                let label = if stage == QuaternionStage::Holding {
                    step.hold_label
                } else {
                    step.transition_label
                };
                ui.weak(label);
            }
            ui.weak("For each quarter-turn, move smoothly at an even speed over about two seconds. The transition is recorded until you start the hold.");

            match stage {
                QuaternionStage::Positioning => self.quaternion_positioning_ui(ui, native_ready),
                QuaternionStage::Holding => self.quaternion_holding_ui(ui, now),
                QuaternionStage::Complete => {
                    ui.colored_label(
                        egui::Color32::LIGHT_GREEN,
                        "All 25 holds are complete. Stop and save the capture below.",
                    );
                    if ui.button("Repeat guided sequence").clicked() {
                        self.begin_quaternion_experiment();
                    }
                }
                QuaternionStage::Idle => {}
            }
        });

        if self.quaternion_experiment.is_running()
            && ui.button("Abort guided sequence (keep capture)").clicked()
        {
            let freeform_label = PHASES[self.motion_capture_phase].label;
            let error = self.motion_capture.as_mut().and_then(|capture| {
                self.quaternion_experiment
                    .abort(capture)
                    .and_then(|()| capture.set_label(freeform_label))
                    .err()
            });
            if let Some(error) = error {
                self.log(format!("Could not abort quaternion experiment: {error}"));
                self.finish_motion_capture(false);
            }
        }

        self.quaternion_experiment.is_engaged()
    }

    fn quaternion_positioning_ui(&mut self, ui: &mut egui::Ui, native_ready: bool) {
        if !native_ready {
            ui.colored_label(
                egui::Color32::YELLOW,
                "Waiting for native 0x0A notifications before the hold can begin…",
            );
        }
        if ui
            .add_enabled(
                native_ready,
                egui::Button::new(format!(
                    "Positioned — start {}-second hold",
                    HOLD_DURATION.as_secs()
                )),
            )
            .clicked()
        {
            let error = self.motion_capture.as_mut().and_then(|capture| {
                self.quaternion_experiment
                    .begin_hold(capture, Instant::now())
                    .err()
            });
            if let Some(error) = error {
                self.log(format!("Could not start quaternion hold: {error}"));
                self.finish_motion_capture(false);
            }
        }
    }

    fn quaternion_holding_ui(&self, ui: &mut egui::Ui, now: Instant) {
        let remaining = self
            .quaternion_experiment
            .remaining(now)
            .unwrap_or_default()
            .as_secs_f32();
        let progress = self
            .quaternion_experiment
            .hold_progress(now)
            .unwrap_or_default();
        ui.strong(format!(
            "Hold completely still — {remaining:.1} s remaining"
        ));
        ui.add(
            egui::ProgressBar::new(progress)
                .desired_width(360.0)
                .show_percentage(),
        );
    }
}
