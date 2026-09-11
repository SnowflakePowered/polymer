use crate::{BluetoothTestApp, DecodedInput};
use eframe::egui;

impl BluetoothTestApp {
    pub(super) fn decoded_input_ui(&self, ui: &mut egui::Ui, input: DecodedInput) {
        ui.heading(format!("Decoded input · {:?}", input.role));
        ui.horizontal_wrapped(|ui| {
            ui.strong("Buttons");
            if input.state.buttons == 0 {
                ui.weak("None");
            }
            ui.monospace(format!("raw {:08X}", input.state.buttons));
        });
        ui.horizontal(|ui| {
            ui.strong("Battery");
            if let Some(level) = input.battery_level {
                ui.label(if input.state.pressure.mask != 0 {
                    format!("{level}%")
                } else {
                    format!("{level}/4")
                });
            } else {
                ui.weak("not reported");
            }
            if input.charging {
                ui.label("charging");
            }
        });
        ui.horizontal(|ui| {
            ui.monospace(format!(
                "Left stick  X {:6}  Y {:6}",
                input.state.left_stick[0], input.state.left_stick[1]
            ));
            ui.monospace(format!(
                "Right stick  X {:6}  Y {:6}",
                input.state.right_stick[0], input.state.right_stick[1]
            ));
        });
        ui.monospace(format!(
            "Triggers  L {:5}  R {:5} · accel {:?} · gyro {:?}",
            input.state.left_trigger,
            input.state.right_trigger,
            input.state.accelerometer,
            input.state.gyroscope,
        ));
        if let Some(timestamp) = input.state.sensor_timestamp_ns {
            ui.monospace(format!(
                "Sensor time: {:.3} ms · accel m/s² · gyro rad/s",
                timestamp as f64 / 1_000_000.0
            ));
        }
        if let Some(contacts) = input.state.touch_contacts {
            ui.label("Touchpad · two contacts (1920 × 1080)");
            for (index, contact) in contacts.iter().enumerate() {
                ui.monospace(format!(
                    "Contact {} · ID {} · {} · X {} Y {}",
                    index + 1,
                    contact.id,
                    if contact.active { "down" } else { "up" },
                    contact.position[0],
                    contact.position[1]
                ));
            }
            let (rect, _) = ui.allocate_exact_size(egui::vec2(288.0, 162.0), egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, 6.0, ui.visuals().extreme_bg_color);
            for (index, contact) in contacts
                .iter()
                .enumerate()
                .filter(|(_, contact)| contact.active)
            {
                let position = rect.min
                    + egui::vec2(
                        (f32::from(contact.position[0]) / 1920.0).clamp(0.0, 1.0) * rect.width(),
                        (f32::from(contact.position[1]) / 1080.0).clamp(0.0, 1.0) * rect.height(),
                    );
                ui.painter().circle_filled(
                    position,
                    6.0,
                    [egui::Color32::LIGHT_BLUE, egui::Color32::LIGHT_GREEN][index],
                );
            }
        }
        if input.state.pressure.mask != 0 {
            use polymer_input::GamepadButton::*;
            ui.label("Pressure · 0–255");
            egui::Grid::new("button-pressure")
                .num_columns(2)
                .show(ui, |ui| {
                    for (button, label) in [
                        (A, "Cross"),
                        (B, "Circle"),
                        (X, "Square"),
                        (Y, "Triangle"),
                        (L, "L1"),
                        (R, "R1"),
                        (DpadUp, "Up"),
                        (DpadRight, "Right"),
                        (DpadDown, "Down"),
                        (DpadLeft, "Left"),
                    ] {
                        let value = input.state.pressure.values[button as usize];
                        ui.label(label);
                        ui.add(
                            egui::ProgressBar::new(f32::from(value) / 255.0)
                                .desired_width(180.0)
                                .text(value.to_string()),
                        );
                        ui.end_row();
                    }
                    for (label, value) in [
                        ("L2", input.state.left_trigger),
                        ("R2", input.state.right_trigger),
                    ] {
                        ui.label(label);
                        ui.add(
                            egui::ProgressBar::new(f32::from(value) / 65535.0)
                                .desired_width(180.0)
                                .text((value / 257).to_string()),
                        );
                        ui.end_row();
                    }
                });
            ui.monospace(format!(
                "Yaw rate: {:.2} °/s",
                input.state.gyroscope[1].to_degrees()
            ));
            ui.weak("DS3 measures yaw only. Turn left/right around the vertical axis; pitch and roll gyro remain zero.");
        }
    }
}
