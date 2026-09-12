use eframe::egui;
use polymer_bluetooth::TransportKind;
use polymer_bluetooth::cable_pairing::{CableDeviceIdentity, CablePairingHost, CablePairingState};

pub mod driver;
pub mod worker;

pub enum Action {
    SwitchDriver(CableDeviceIdentity),
    Enumerate,
    Inspect(CableDeviceIdentity),
    Pair {
        device: CableDeviceIdentity,
        expected: CablePairingState,
    },
}

pub enum Outcome {
    DriverSwitched(String),
    Devices(Vec<CableDeviceIdentity>),
    Inspected {
        device: CableDeviceIdentity,
        state: CablePairingState,
    },
    Paired {
        device: CableDeviceIdentity,
        state: CablePairingState,
    },
}

pub enum Event {
    Host(CablePairingHost),
    BleHost(CablePairingHost),
    RegistryUnavailable(String),
    Finished(Result<Outcome, String>),
}

#[derive(Default)]
pub struct CablePairingPanel {
    host: Option<CablePairingHost>,
    ble_host: Option<CablePairingHost>,
    devices: Vec<CableDeviceIdentity>,
    inspected: Option<(CableDeviceIdentity, CablePairingState)>,
    busy: bool,
    status: String,
    failed: bool,
    registry_error: Option<String>,
}

impl CablePairingPanel {
    pub fn handle(&mut self, event: Event) -> Option<String> {
        match event {
            Event::Host(host) => {
                self.host = Some(host);
                return None;
            }
            Event::BleHost(host) => {
                self.ble_host = Some(host);
                return None;
            }
            Event::RegistryUnavailable(error) => {
                self.registry_error = Some(error);
                return None;
            }
            Event::Finished(result) => {
                self.busy = false;
                self.failed = result.is_err();
                match result {
                    Ok(Outcome::DriverSwitched(message)) => {
                        self.devices.clear();
                        self.inspected = None;
                        self.status = message;
                    }
                    Ok(Outcome::Devices(devices)) => {
                        self.inspected = None;
                        self.status = if devices.is_empty() {
                            "No supported USB controllers found. Connect a DS3, DS4, DualSense, Switch Pro, or Switch 2 controller with a data cable and refresh.".into()
                        } else {
                            format!(
                                "Found {} USB controller(s). Read addresses before pairing.",
                                devices.len()
                            )
                        };
                        self.devices = devices;
                    }
                    Ok(Outcome::Inspected { device, state }) => {
                        self.status = "Addresses read. Pairing will replace the controller's saved wireless host.".into();
                        self.inspected = Some((device, state));
                    }
                    Ok(Outcome::Paired { device, state }) => {
                        self.status = format!(
                            "Verified and saved wireless pairing for {}. Unplug USB and press the controller's HOME/PS button to connect.",
                            state.controller_address,
                        );
                        self.inspected = Some((device, state));
                    }
                    Err(error) => {
                        // A failed write/readback may have changed the pad. Require
                        // another inspection before offering another write.
                        self.inspected = None;
                        self.status = error;
                    }
                }
            }
        }
        Some(format!("USB cable pairing: {}", self.status))
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        let mut action = None;
        egui::CollapsingHeader::new("USB cable pairing")
            .default_open(true)
            .show(ui, |ui| {
                ui.label("Provision a persistent wireless pairing over USB. Supported: DualShock 3 / SIXAXIS, DualShock 4, DualSense, Switch Pro, Joy-Con 2, Switch 2 Pro, and NSO GameCube.");
                ui.label("On Windows, DS3 pairing requires WinUSB on the controller. Switching its driver can affect wired input. The switch button attempts it without requesting elevation.");
                if let Some(host) = self.host {
                    ui.monospace(format!("Dedicated BR/EDR host: {}", host.address));
                } else {
                    ui.weak("Dedicated BR/EDR adapter unavailable. USB addresses can still be inspected.");
                }
                if let Some(host) = self.ble_host {
                    ui.monospace(format!("OS BLE host: {}", host.address));
                } else {
                    ui.weak("OS BLE adapter unavailable. Switch 2 pairing is disabled.");
                }
                ui.label("DualShock 4 and DualSense pair through the standard USB HID driver.");
                ui.label("Switch 2 uses USB command interface 1 and pairs to the OS BLE adapter.");
                if let Some(error) = &self.registry_error {
                    ui.colored_label(egui::Color32::LIGHT_RED, format!("Pairing disabled: persistent registry unavailable: {error}"));
                }
                if ui.add_enabled(!self.busy, egui::Button::new("Refresh USB controllers")).clicked() {
                    action = Some(Action::Enumerate);
                }
                ui.add_enabled_ui(!self.busy, |ui| {
                    self.devices_ui(ui, &mut action);
                    self.pairing_ui(ui, &mut action);
                });
                if self.busy { ui.spinner(); }
                if self.failed {
                    ui.colored_label(egui::Color32::LIGHT_RED, &self.status);
                } else {
                    ui.label(&self.status);
                }
            });
        if action.is_some() {
            self.busy = true;
            self.failed = false;
            self.status = "Performing USB pairing operation…".into();
        }
        action
    }

    fn devices_ui(&self, ui: &mut egui::Ui, action: &mut Option<Action>) {
        for device in &self.devices {
            ui.horizontal(|ui| {
                ui.label(format!(
                    "{} · {:04X}:{:04X} · {}",
                    device.name,
                    device.vendor_id,
                    device.product_id,
                    device.access_label()
                ))
                .on_hover_text(device.path().to_string_lossy());
                if ui.button("Read addresses").clicked() {
                    *action = Some(Action::Inspect(device.clone()));
                }
                if cfg!(windows)
                    && device.profile_id.as_str() == "sony.dualshock3"
                    && !device.uses_winusb()
                    && !device.uses_dshidmini()
                    && ui.button("Switch to WinUSB (no elevation)").clicked()
                {
                    *action = Some(Action::SwitchDriver(device.clone()));
                }
            });
        }
    }

    fn pairing_ui(&self, ui: &mut egui::Ui, action: &mut Option<Action>) {
        let Some((device, state)) = &self.inspected else {
            return;
        };
        ui.separator();
        ui.label(&device.name);
        ui.monospace(format!("Controller: {}", state.controller_address));
        ui.monospace(format!(
            "Saved host: {}",
            state
                .host_address
                .map_or_else(|| "none".into(), |address| address.to_string())
        ));
        let ble = device.wireless_transport() == TransportKind::Ble;
        let host = if ble { self.ble_host } else { self.host };
        if ui
            .add_enabled(
                host.is_some() && self.registry_error.is_none(),
                egui::Button::new(if ble {
                    "Pair to OS BLE adapter"
                } else {
                    "Pair to dedicated adapter"
                }),
            )
            .clicked()
        {
            *action = Some(Action::Pair {
                device: device.clone(),
                expected: *state,
            });
        }
    }
}
