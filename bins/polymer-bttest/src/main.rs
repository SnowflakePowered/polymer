mod cable_pairing;
mod controller_model;
mod decoded_input;
mod input;
mod motion_capture;
mod worker;

use std::{
    collections::{BTreeMap, VecDeque},
    sync::mpsc,
    time::{Duration, Instant},
};

use crate::input::{
    decoder::{DecodedInput, MotionFrame},
    wiimote::{
        Accelerometer, Gyroscope, Telemetry as WiiTelemetry,
        airmouse::{AirMouse, PointerPosition},
    },
};
use controller_model::GameCubeControllerModel;
use eframe::egui;
use motion_capture::{
    COMMON_REPORT, GAMECUBE_REPORT, MotionCapture, PHASES,
    class_c_acceleration::{AccelerationPacket, ClassCAccelerationExperiment},
    class_d::{ClassDExperiment, ClassDPacket},
    lanes::{ClassCPacket, LaneExperiment},
    quaternion::QuaternionExperiment,
    rare_timing::RareTimingExperiment,
    timing::{TimingExperiment, TimingPacket},
};
use polymer_bluetooth::{DeviceAddr, transport::ble::BleDevice};
use worker::{BondingMode, Command, DeviceView, Event, RadioKind};

const DOUBLE_HOME_WINDOW: Duration = Duration::from_millis(700);
const DUAL_REPORT_PROBE_DURATION: Duration = Duration::from_secs(3);
const REPORT_ALTERNATION_INTERVAL: Duration = Duration::from_millis(250);

fn main() -> eframe::Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(
            "warn,polymer_bluetooth::transport::bredr::l2cap::link=debug",
        )
    });
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .try_init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([920.0, 820.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Polymer Bluetooth Sync Test",
        options,
        Box::new(|context| {
            context.egui_ctx.set_visuals(egui::Visuals::dark());
            Ok(Box::new(BluetoothTestApp::new()))
        }),
    )
}

struct BluetoothTestApp {
    cable_pairing: cable_pairing::CablePairingPanel,
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
    events: mpsc::Receiver<Event>,
    status: String,
    br_ready: bool,
    ble_adapters: Vec<String>,
    ble_adapter_index: usize,
    radio: RadioKind,
    busy: bool,
    br_scanning: bool,
    ble_scanning: bool,
    listening: bool,
    controller_order: bool,
    connected: BTreeMap<String, ConnectedSession>,
    selected_connection: Option<String>,
    devices: Vec<DeviceView>,
    selected: Option<usize>,
    ble_devices: Vec<BleDevice>,
    ble_selected: Option<usize>,
    pin: BondingMode,
    output_report: String,
    controller_inputs: BTreeMap<String, ControllerInputView>,
    air_mouse: AirMouse,
    pointer_position: PointerPosition,
    pointer_source: Option<PointerSource>,
    last_accelerometer: Option<Accelerometer>,
    last_gyroscope: Option<Gyroscope>,
    home_was_pressed: bool,
    last_home_press: Option<Instant>,
    logs: VecDeque<String>,
    motion_capture: Option<MotionCapture>,
    motion_experiment: MotionExperiment,
    lane_experiment: LaneExperiment,
    class_c_acceleration_experiment: ClassCAccelerationExperiment,
    class_d_experiment: ClassDExperiment,
    timing_experiment: TimingExperiment,
    rare_timing_experiment: RareTimingExperiment,
    quaternion_experiment: QuaternionExperiment,
    motion_capture_phase: usize,
    last_motion_capture_path: Option<std::path::PathBuf>,
    controller_model: GameCubeControllerModel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PointerSource {
    IrCamera,
    AirMouse,
}

#[derive(Clone, Copy, Debug)]
enum MotionExperiment {
    Idle,
    AwaitingDualSubscription,
    Probing {
        deadline: Instant,
        common_start: u64,
        native_start: u64,
    },
    Concurrent,
    Alternating {
        next_switch_at: Instant,
        next_report: u8,
        pending: bool,
    },
}

impl MotionExperiment {
    const fn is_active(self) -> bool {
        !matches!(self, Self::Idle)
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::AwaitingDualSubscription => "subscribing both characteristics…",
            Self::Probing { .. } => "probing simultaneous notifications…",
            Self::Concurrent => "both report streams are live",
            Self::Alternating { pending: true, .. } => "alternating · switching…",
            Self::Alternating { pending: false, .. } => "alternating reports every 250 ms",
        }
    }
}

#[derive(Clone, Default)]
struct ControllerInputView {
    report_count: u64,
    last_report: String,
    wii_telemetry: Option<WiiTelemetry>,
    decoded_input: Option<DecodedInput>,
    motion: Option<MotionFrame>,
    orientation_reference: Option<[f32; 4]>,
}

#[derive(Clone)]
struct ConnectedSession {
    name: String,
    radio: RadioKind,
    slot: usize,
}

impl PointerSource {
    fn label(self) -> &'static str {
        match self {
            Self::IrCamera => "IR camera · absolute",
            Self::AirMouse => "Air mouse · accel + MotionPlus",
        }
    }
}

impl BluetoothTestApp {
    fn new() -> Self {
        let (commands, events) = worker::spawn();
        Self {
            commands,
            events,
            cable_pairing: cable_pairing::CablePairingPanel::default(),
            status: "Opening dedicated adapter…".into(),
            br_ready: false,
            ble_adapters: Vec::new(),
            ble_adapter_index: 0,
            radio: RadioKind::Ble,
            busy: true,
            br_scanning: false,
            ble_scanning: false,
            listening: true,
            controller_order: false,
            connected: BTreeMap::new(),
            selected_connection: None,
            devices: Vec::new(),
            selected: None,
            ble_devices: Vec::new(),
            ble_selected: None,
            pin: BondingMode::Ssp,
            output_report: String::new(),
            controller_inputs: BTreeMap::new(),
            air_mouse: AirMouse::new(),
            pointer_position: PointerPosition::CENTER,
            pointer_source: None,
            last_accelerometer: None,
            last_gyroscope: None,
            home_was_pressed: false,
            last_home_press: None,
            logs: VecDeque::new(),
            motion_capture: None,
            motion_experiment: MotionExperiment::Idle,
            lane_experiment: LaneExperiment::default(),
            class_c_acceleration_experiment: ClassCAccelerationExperiment::default(),
            class_d_experiment: ClassDExperiment::default(),
            timing_experiment: TimingExperiment::default(),
            rare_timing_experiment: RareTimingExperiment::default(),
            quaternion_experiment: QuaternionExperiment::default(),
            motion_capture_phase: 0,
            last_motion_capture_path: None,
            controller_model: GameCubeControllerModel::new(),
        }
    }

    fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::CablePairing(event) => {
                    if let Some(message) = self.cable_pairing.handle(event) {
                        self.log(message);
                    }
                }
                Event::BrReady(message) => {
                    self.br_ready = true;
                    self.busy = false;
                    self.status = message.clone();
                    self.log(message);
                }
                Event::BleReady(adapters) => {
                    self.busy = false;
                    self.status = format!("BLE ready: {}", adapters.join(", "));
                    for adapter in &adapters {
                        self.log(format!("OS BLE adapter: {adapter}"));
                    }
                    self.ble_adapters = adapters;
                }
                Event::Scanning {
                    radio,
                    enabled,
                    message,
                } => {
                    match radio {
                        RadioKind::BrEdr => self.br_scanning = enabled,
                        RadioKind::Ble => self.ble_scanning = enabled,
                    }
                    self.status = message.clone();
                    self.log(message);
                }
                Event::Busy(message) => {
                    self.busy = true;
                    self.status = message.clone();
                    self.log(message);
                }
                Event::Idle(message) => {
                    self.busy = false;
                    self.status = message.clone();
                    self.log(message);
                }
                Event::Device(device) => {
                    let automatic = (self.controller_order
                        && !self.connected.contains_key(&device.addr))
                    .then(|| {
                        device
                            .profile_id
                            .as_ref()
                            .map(|profile_id| Command::ConnectBr {
                                device: device.inquiry.clone(),
                                pin: if device.is_wii_remote() {
                                    BondingMode::WiiSync
                                } else {
                                    BondingMode::Ssp
                                },
                                profile_id: Some(profile_id.clone()),
                            })
                    });
                    if let Some(existing) = self
                        .devices
                        .iter_mut()
                        .find(|known| known.addr == device.addr)
                    {
                        *existing = device;
                    } else {
                        self.devices.push(device);
                    }
                    if let Some(Some(command)) = automatic {
                        self.send(command);
                    }
                }
                Event::BleDevice(device) => {
                    let automatic = (self.controller_order
                        && !self.connected.contains_key(&device.addr.to_string()))
                    .then(|| Command::ConnectBle {
                        adapter_index: self.ble_adapter_index,
                        device: device.clone(),
                    });
                    if let Some(existing) = self
                        .ble_devices
                        .iter_mut()
                        .find(|known| known.addr == device.addr)
                    {
                        *existing = device;
                    } else {
                        self.ble_devices.push(device);
                    }
                    if let Some(command) = automatic {
                        self.send(command);
                    }
                }
                Event::Connected {
                    addr,
                    name,
                    radio,
                    slot,
                } => {
                    let resumed_capture_report = self.motion_capture.as_mut().and_then(|capture| {
                        (capture.address() == addr).then(|| {
                            let report_id = capture.selected_report();
                            capture
                                .mark_transport("transport_reconnected", None)
                                .map(|()| report_id)
                        })
                    });
                    self.connected
                        .insert(addr.clone(), ConnectedSession { name, radio, slot });
                    self.controller_inputs.entry(addr.clone()).or_default();
                    self.selected_connection = Some(addr.clone());
                    self.radio = radio;
                    self.busy = false;
                    self.status = format!("{} controller(s) connected", self.connected.len());
                    self.log(format!(
                        "HID channels ready for {addr} in player slot {}",
                        slot + 1
                    ));
                    match resumed_capture_report {
                        Some(Ok(report_id)) => {
                            if let Ok(addr) = addr.parse() {
                                self.send(Command::SelectSwitch2InputReport { addr, report_id });
                            }
                        }
                        Some(Err(error)) => {
                            self.log(format!("Motion capture reconnect marker failed: {error}"));
                            self.finish_motion_capture(false);
                        }
                        None => {}
                    }
                }
                Event::Disconnected { addr, reason } => {
                    let is_capture_controller = self
                        .motion_capture
                        .as_ref()
                        .is_some_and(|capture| capture.address() == addr);
                    let preserve_sleep_capture =
                        is_capture_controller && self.lane_experiment.awaits_sleep_reconnect();
                    if preserve_sleep_capture {
                        let marker_error = self.motion_capture.as_mut().and_then(|capture| {
                            capture
                                .mark_transport("transport_disconnected", Some(&reason))
                                .err()
                        });
                        if let Some(error) = marker_error {
                            self.log(format!("Motion capture disconnect marker failed: {error}"));
                            self.finish_motion_capture(false);
                        }
                    } else if is_capture_controller {
                        self.finish_motion_capture(false);
                    }
                    self.connected.remove(&addr);
                    self.controller_inputs.remove(&addr);
                    if self.selected_connection.as_deref() == Some(addr.as_str()) {
                        self.selected_connection = self.connected.keys().next().cloned();
                    }
                    if self.connected.is_empty() {
                        self.air_mouse = AirMouse::new();
                        self.pointer_position = PointerPosition::CENTER;
                        self.pointer_source = None;
                        self.last_accelerometer = None;
                        self.last_gyroscope = None;
                        self.home_was_pressed = false;
                        self.last_home_press = None;
                    }
                    self.busy = false;
                    self.status = if self.connected.is_empty() {
                        reason.clone()
                    } else {
                        format!("{} controller(s) connected", self.connected.len())
                    };
                    self.log(format!("{addr}: {reason}"));
                }
                Event::Report { addr, bytes } => {
                    let input = self.controller_inputs.entry(addr).or_default();
                    input.report_count += 1;
                    input.last_report = hex(&bytes);
                }
                Event::BleReport {
                    addr,
                    characteristic,
                    bytes,
                    received_at,
                } => {
                    let input = self.controller_inputs.entry(addr.clone()).or_default();
                    input.report_count += 1;
                    input.last_report = hex(&bytes);
                    let mut capture_error = self.motion_capture.as_mut().and_then(|capture| {
                        capture
                            .record_notification(&addr, &characteristic, &bytes, received_at)
                            .err()
                    });
                    if capture_error.is_none()
                        && let Some(packet) = ClassCPacket::parse(&characteristic, &bytes)
                        && let Some(capture) = self.motion_capture.as_mut()
                    {
                        capture_error =
                            self.lane_experiment.observe_class_c(capture, &packet).err();
                    }
                    if capture_error.is_none()
                        && let Some(packet) = ClassDPacket::parse(&characteristic, &bytes)
                        && let Some(capture) = self.motion_capture.as_mut()
                    {
                        capture_error = self.class_d_experiment.observe(capture, &packet).err();
                    }
                    if capture_error.is_none()
                        && let Some(packet) = AccelerationPacket::parse(&characteristic, &bytes)
                        && let Some(capture) = self.motion_capture.as_mut()
                    {
                        capture_error = self
                            .class_c_acceleration_experiment
                            .observe(capture, packet, received_at)
                            .err();
                    }
                    if capture_error.is_none()
                        && let Some(packet) = TimingPacket::parse(&characteristic, &bytes)
                        && let Some(capture) = self.motion_capture.as_mut()
                    {
                        capture_error = self
                            .timing_experiment
                            .observe(capture, packet, received_at)
                            .err();
                        if capture_error.is_none() {
                            capture_error = self
                                .rare_timing_experiment
                                .observe(capture, packet, received_at)
                                .err();
                        }
                    }
                    if let Some(error) = capture_error {
                        self.log(format!("Motion capture write failed: {error}"));
                        self.finish_motion_capture(false);
                    }
                }
                Event::BleReportsDropped { addr, count } => {
                    let capture_error = self
                        .motion_capture
                        .as_mut()
                        .and_then(|capture| capture.record_drop(&addr, count).err());
                    if let Some(error) = capture_error {
                        self.log(format!("Motion capture drop marker failed: {error}"));
                        self.finish_motion_capture(false);
                    }
                    self.log(format!("Dropped {count} raw BLE notifications from {addr}"));
                }
                Event::Switch2InputReportSelected { addr, report_id } => {
                    let is_capture_controller = self
                        .motion_capture
                        .as_ref()
                        .is_some_and(|capture| capture.address() == addr);
                    let capture_error = self.motion_capture.as_mut().and_then(|capture| {
                        (capture.address() == addr)
                            .then(|| capture.set_selected_report(report_id).err())
                            .flatten()
                    });
                    if let Some(error) = capture_error {
                        self.log(format!("Motion capture report marker failed: {error}"));
                        self.finish_motion_capture(false);
                    }
                    if is_capture_controller
                        && let MotionExperiment::Alternating {
                            next_switch_at,
                            next_report,
                            pending,
                        } = &mut self.motion_experiment
                    {
                        *next_report = if report_id == COMMON_REPORT {
                            GAMECUBE_REPORT
                        } else {
                            COMMON_REPORT
                        };
                        *next_switch_at = Instant::now() + REPORT_ALTERNATION_INTERVAL;
                        *pending = false;
                    }
                }
                Event::Switch2InputReportSelectionFailed {
                    addr,
                    report_id,
                    reason,
                } => {
                    if self
                        .motion_capture
                        .as_ref()
                        .is_some_and(|capture| capture.address() == addr)
                        && let MotionExperiment::Alternating {
                            next_switch_at,
                            pending,
                            ..
                        } = &mut self.motion_experiment
                    {
                        *next_switch_at = Instant::now() + REPORT_ALTERNATION_INTERVAL;
                        *pending = false;
                        self.log(format!(
                            "Automatic 0x{report_id:02X} switch failed and will retry: {reason}"
                        ));
                    }
                }
                Event::Switch2InputReportsSubscribed { addr, report_ids } => {
                    if self
                        .motion_capture
                        .as_ref()
                        .is_some_and(|capture| capture.address() == addr)
                        && matches!(
                            self.motion_experiment,
                            MotionExperiment::AwaitingDualSubscription
                        )
                    {
                        let both_present = report_ids.contains(&COMMON_REPORT)
                            && report_ids.contains(&GAMECUBE_REPORT);
                        if both_present {
                            let capture = self.motion_capture.as_ref().expect("checked above");
                            self.motion_experiment = MotionExperiment::Probing {
                                deadline: Instant::now() + DUAL_REPORT_PROBE_DURATION,
                                common_start: capture.common_samples(),
                                native_start: capture.native_samples(),
                            };
                            self.mark_motion_experiment("dual_subscription_active");
                        } else {
                            self.mark_motion_experiment("dual_subscription_incomplete");
                            self.begin_motion_alternation();
                        }
                    }
                }
                Event::Switch2InputReportsSubscriptionFailed { addr, reason } => {
                    if self
                        .motion_capture
                        .as_ref()
                        .is_some_and(|capture| capture.address() == addr)
                        && matches!(
                            self.motion_experiment,
                            MotionExperiment::AwaitingDualSubscription
                        )
                    {
                        self.log(format!(
                            "Dual-report subscription failed; using automatic alternation: {reason}"
                        ));
                        self.mark_motion_experiment("dual_subscription_failed");
                        self.begin_motion_alternation();
                    }
                }
                Event::WiiTelemetry {
                    addr,
                    telemetry,
                    received_at,
                } => self.handle_wii_telemetry(addr, telemetry, received_at),
                Event::DecodedInput { addr, input } => {
                    let view = self.controller_inputs.entry(addr).or_default();
                    if let Some(motion) = input.motion {
                        view.motion = Some(motion);
                        view.orientation_reference.get_or_insert(motion.orientation);
                    }
                    view.decoded_input = Some(input);
                }
                Event::Log(message) => self.log(message),
                Event::Error(message) => {
                    self.busy = false;
                    self.status = format!("Error: {message}");
                    self.log(format!("ERROR: {message}"));
                }
            }
        }
    }

    fn log(&mut self, message: String) {
        eprintln!("{message}");
        if self.logs.len() == 300 {
            self.logs.pop_front();
        }
        self.logs.push_back(message);
    }

    fn handle_wii_telemetry(
        &mut self,
        addr: String,
        telemetry: WiiTelemetry,
        received_at: Instant,
    ) {
        self.controller_inputs
            .entry(addr)
            .or_default()
            .wii_telemetry = Some(telemetry);
        self.last_accelerometer = telemetry.accelerometer;
        self.last_gyroscope = telemetry.gyroscope;

        let home_pressed = telemetry.home_pressed();
        if home_pressed
            && !self.home_was_pressed
            && register_home_press(&mut self.last_home_press, received_at)
        {
            self.air_mouse.center();
            self.log("Double Home: air mouse recentered".into());
        }
        self.home_was_pressed = home_pressed;

        if let Some(ir_position) = ir_pointer_position(telemetry) {
            self.pointer_position = ir_position;
            self.pointer_source = Some(PointerSource::IrCamera);
            self.air_mouse
                .track_absolute_position(ir_position, received_at);
        } else if let (Some(accelerometer), Some(gyroscope)) =
            (telemetry.accelerometer, telemetry.gyroscope)
        {
            let position =
                self.air_mouse
                    .next_pointer_position(accelerometer, gyroscope, received_at);
            self.pointer_position = position;
            self.pointer_source = Some(PointerSource::AirMouse);
        }
    }
}

impl eframe::App for BluetoothTestApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();
        self.tick_motion_experiment();
        self.tick_quaternion_experiment();
        let repaint_interval = if self
            .controller_inputs
            .values()
            .any(|input| input.motion.is_some())
        {
            Duration::from_millis(16)
        } else {
            Duration::from_millis(50)
        };
        context.request_repaint_after(repaint_interval);

        egui::TopBottomPanel::top("status").show(context, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Polymer Bluetooth Sync Test");
                ui.separator();
                let color = if self.status.starts_with("Error") {
                    egui::Color32::LIGHT_RED
                } else if !self.connected.is_empty() {
                    egui::Color32::LIGHT_GREEN
                } else {
                    egui::Color32::LIGHT_BLUE
                };
                ui.colored_label(color, &self.status);
            });
        });

        egui::CentralPanel::default().show(context, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                if self.controller_order {
                    self.controller_order_ui(ui);
                    return;
                }
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.radio, RadioKind::Ble, "Switch 2 BLE");
                    ui.selectable_value(&mut self.radio, RadioKind::BrEdr, "Classic BR/EDR");
                });
                ui.separator();
                ui.columns(2, |columns| {
                    self.discovery_ui(&mut columns[0]);
                    self.connection_ui(&mut columns[1]);
                });
                if self.radio == RadioKind::BrEdr
                    && let Some(action) = self.cable_pairing.show(ui)
                {
                    self.send(Command::CablePairing(action));
                }
                self.motion_capture_ui(ui);
                if !self.controller_inputs.is_empty() {
                    ui.separator();
                    ui.heading("Controller inputs");
                    let inputs = self
                        .controller_inputs
                        .iter()
                        .map(|(addr, input)| (addr.clone(), input.clone()))
                        .collect::<Vec<_>>();
                    for (addr, input) in inputs {
                        let controller_name = self
                            .connected
                            .get(&addr)
                            .map(|session| session.name.clone())
                            .unwrap_or_else(|| "Switch 2 controller".into());
                        egui::CollapsingHeader::new(format!(
                            "{addr} · {} reports",
                            input.report_count
                        ))
                        .id_salt(("controller-input", &addr))
                        .default_open(true)
                        .show(ui, |ui| {
                            if input.last_report.is_empty() {
                                ui.weak("Waiting for input reports");
                            } else {
                                ui.monospace(format!("Raw HID  {}", input.last_report));
                            }
                            if let Some(telemetry) = input.wii_telemetry {
                                ui.separator();
                                self.wii_telemetry_ui(ui, telemetry);
                            }
                            if let Some(decoded) = input.decoded_input {
                                ui.separator();
                                self.decoded_input_ui(ui, decoded);
                            }
                            if let Some(motion) = input.motion {
                                ui.separator();
                                self.switch2_motion_ui(
                                    ui,
                                    &addr,
                                    &controller_name,
                                    motion,
                                    input.orientation_reference,
                                );
                            }
                        });
                    }
                }
                ui.separator();
                ui.heading("Event log");
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .max_height(210.0)
                    .show(ui, |ui| {
                        for line in &self.logs {
                            ui.monospace(line);
                        }
                    });
            });
        });
        if !self.controller_order {
            self.pointer_target_window(context);
        }
    }
}

impl BluetoothTestApp {
    fn begin_motion_experiment(&mut self) {
        let Some(addr) = self
            .motion_capture
            .as_ref()
            .and_then(|capture| capture.address().parse().ok())
        else {
            return;
        };
        self.motion_experiment = MotionExperiment::AwaitingDualSubscription;
        self.mark_motion_experiment("dual_subscription_requested");
        self.send(Command::SubscribeAllSwitch2InputReports { addr });
    }

    fn begin_motion_alternation(&mut self) {
        let Some(addr) = self
            .motion_capture
            .as_ref()
            .and_then(|capture| capture.address().parse().ok())
        else {
            self.motion_experiment = MotionExperiment::Idle;
            return;
        };
        self.motion_experiment = MotionExperiment::Alternating {
            next_switch_at: Instant::now() + REPORT_ALTERNATION_INTERVAL,
            next_report: GAMECUBE_REPORT,
            pending: true,
        };
        self.mark_motion_experiment("automatic_alternation_started");
        self.send(Command::SelectSwitch2InputReport {
            addr,
            report_id: COMMON_REPORT,
        });
    }

    fn stop_motion_experiment(&mut self) {
        if !self.motion_experiment.is_active() {
            return;
        }
        self.mark_motion_experiment("experiment_stopped");
        self.motion_experiment = MotionExperiment::Idle;
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
    }

    fn mark_motion_experiment(&mut self, event: &str) {
        let error = self
            .motion_capture
            .as_mut()
            .and_then(|capture| capture.mark_experiment(event).err());
        if let Some(error) = error {
            self.log(format!("Could not write motion experiment marker: {error}"));
            self.finish_motion_capture(false);
        }
    }

    fn tick_motion_experiment(&mut self) {
        let now = Instant::now();
        let sample_counts = self
            .motion_capture
            .as_ref()
            .map(|capture| (capture.common_samples(), capture.native_samples()));
        let mut marker = None;
        let mut requested_report = None;
        match &mut self.motion_experiment {
            MotionExperiment::Probing {
                deadline,
                common_start,
                native_start,
            } if now >= *deadline => {
                let simultaneous = sample_counts.is_some_and(|(common, native)| {
                    common > *common_start && native > *native_start
                });
                if simultaneous {
                    self.motion_experiment = MotionExperiment::Concurrent;
                    marker = Some("simultaneous_reports_confirmed");
                } else {
                    marker = Some("simultaneous_reports_not_observed");
                }
            }
            MotionExperiment::Alternating {
                next_switch_at,
                next_report,
                pending,
            } if now >= *next_switch_at && !*pending => {
                requested_report = Some(*next_report);
                *pending = true;
            }
            _ => {}
        }
        if let Some(marker) = marker {
            self.mark_motion_experiment(marker);
            if marker == "simultaneous_reports_not_observed" {
                self.begin_motion_alternation();
            }
        }
        if let Some(report_id) = requested_report
            && let Some(addr) = self
                .motion_capture
                .as_ref()
                .and_then(|capture| capture.address().parse().ok())
        {
            self.send(Command::SelectSwitch2InputReport { addr, report_id });
        }
    }

    fn motion_capture_ui(&mut self, ui: &mut egui::Ui) {
        let selected_gamecube = self.selected_connection.as_ref().and_then(|addr| {
            self.connected.get(addr).and_then(|session| {
                (session.radio == RadioKind::Ble && session.name.contains("GameCube"))
                    .then(|| addr.clone())
            })
        });
        if selected_gamecube.is_none() && self.motion_capture.is_none() {
            return;
        }

        ui.separator();
        ui.heading("NSO GameCube motion capture");
        ui.label("Capture packed native motion for the guided quaternion or class-C lane experiments, or use the freeform controls for report comparison.");

        if self.motion_capture.is_none() {
            let mut start_freeform = false;
            let mut start_guided = false;
            let mut start_lanes = false;
            let mut start_class_c_acceleration = false;
            let mut start_class_d = false;
            let mut start_timing = false;
            let mut start_rare_timing = false;
            let mut start_sleep = false;
            ui.horizontal_wrapped(|ui| {
                start_guided = ui
                    .add_enabled(
                        selected_gamecube.is_some(),
                        egui::Button::new("Start guided quaternion capture…"),
                    )
                    .clicked();
                start_lanes = ui
                    .add_enabled(
                        selected_gamecube.is_some(),
                        egui::Button::new("Start class-C lane capture…"),
                    )
                    .clicked();
                start_class_c_acceleration = ui
                    .add_enabled(
                        selected_gamecube.is_some(),
                        egui::Button::new("Start class-C acceleration filter capture…"),
                    )
                    .clicked();
                start_class_d = ui
                    .add_enabled(
                        selected_gamecube.is_some(),
                        egui::Button::new("Start class-D capture…"),
                    )
                    .clicked();
                start_timing = ui
                    .add_enabled(
                        selected_gamecube.is_some(),
                        egui::Button::new("Start bit 287 + timing capture…"),
                    )
                    .clicked();
                start_rare_timing = ui
                    .add_enabled(
                        selected_gamecube.is_some(),
                        egui::Button::new("Start rare-span timing capture…"),
                    )
                    .clicked();
                start_sleep = ui
                    .add_enabled(
                        selected_gamecube.is_some(),
                        egui::Button::new("Start sleep/wake capture…"),
                    )
                    .clicked();
                start_freeform = ui
                    .add_enabled(
                        selected_gamecube.is_some(),
                        egui::Button::new("Start freeform capture…"),
                    )
                    .clicked();
            });
            if let Some(addr) = selected_gamecube.as_deref() {
                if start_guided {
                    self.start_motion_capture(addr, true);
                } else if start_lanes {
                    self.start_motion_capture(addr, false);
                    self.begin_lane_experiment();
                } else if start_class_c_acceleration {
                    self.start_motion_capture(addr, false);
                    self.begin_class_c_acceleration_experiment();
                } else if start_class_d {
                    self.start_motion_capture(addr, false);
                    self.begin_class_d_experiment();
                } else if start_timing {
                    self.start_motion_capture(addr, false);
                    self.begin_timing_experiment();
                } else if start_rare_timing {
                    self.start_motion_capture(addr, false);
                    self.begin_rare_timing_experiment();
                } else if start_sleep {
                    self.start_motion_capture(addr, false);
                    self.begin_sleep_experiment();
                } else if start_freeform {
                    self.start_motion_capture(addr, false);
                }
            }
            if let Some(path) = &self.last_motion_capture_path {
                ui.monospace(format!("Last capture: {}", path.display()));
            }
            return;
        }

        let capture = self.motion_capture.as_ref().expect("checked above");
        let selected_report = capture.selected_report();
        ui.monospace(capture.path().display().to_string());
        ui.horizontal(|ui| {
            ui.strong(format!("{} packets", capture.samples()));
            ui.label(format!("0x05: {}", capture.common_samples()));
            ui.label(format!("0x0A: {}", capture.native_samples()));
            ui.label(format!("active: 0x{:02X}", capture.selected_report()));
            match capture.observed_report() {
                Some(report_id) if report_id == capture.selected_report() => {
                    ui.colored_label(
                        egui::Color32::LIGHT_GREEN,
                        format!("observed: 0x{report_id:02X}"),
                    );
                }
                Some(report_id) => {
                    ui.colored_label(
                        egui::Color32::LIGHT_RED,
                        format!("observed: 0x{report_id:02X} (mismatch)"),
                    );
                }
                None => {
                    ui.weak("observed: waiting");
                }
            }
        });
        let primary_modes = capture.primary_mode_counts();
        let secondary_modes = capture.secondary_mode_counts();
        ui.monospace(format!(
            "packed bits 0–1     v0 {} · v1 {} · v2 {} · v3 {}",
            primary_modes[0], primary_modes[1], primary_modes[2], primary_modes[3]
        ));
        ui.monospace(format!(
            "packed bits 128–129 v0 {} · v1 {} · v2 {} · v3 {}",
            secondary_modes[0], secondary_modes[1], secondary_modes[2], secondary_modes[3]
        ));

        if self.class_c_acceleration_capture_ui(ui)
            || self.rare_timing_capture_ui(ui)
            || self.timing_capture_ui(ui)
            || self.class_d_capture_ui(ui)
            || self.lane_capture_ui(ui)
            || self.quaternion_capture_ui(ui)
        {
            if ui.button("Stop and save capture").clicked() {
                self.finish_motion_capture(true);
            }
            return;
        }

        let experiment_active = self.motion_experiment.is_active();
        let mut start_experiment = false;
        let mut stop_experiment = false;
        ui.horizontal(|ui| {
            if experiment_active {
                if ui.button("Stop report experiment").clicked() {
                    stop_experiment = true;
                }
            } else if ui.button("Run dual-report experiment").clicked() {
                start_experiment = true;
            }
            ui.label(self.motion_experiment.label());
        });
        ui.weak("The test subscribes 0x05 and 0x0A together for three seconds. If both do not produce notifications, it automatically alternates them every 250 ms.");

        let mut requested_report = None;
        ui.add_enabled_ui(!experiment_active, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(selected_report == COMMON_REPORT, "Common 0x05")
                    .clicked()
                {
                    requested_report = Some(COMMON_REPORT);
                }
                if ui
                    .selectable_label(selected_report == GAMECUBE_REPORT, "Packed native 0x0A")
                    .clicked()
                {
                    requested_report = Some(GAMECUBE_REPORT);
                }
            });
        });

        let previous_phase = self.motion_capture_phase;
        egui::ComboBox::from_label("Manipulation phase")
            .selected_text(PHASES[self.motion_capture_phase].label)
            .show_ui(ui, |ui| {
                for (index, phase) in PHASES.iter().enumerate() {
                    ui.selectable_value(&mut self.motion_capture_phase, index, phase.label);
                }
            });
        ui.colored_label(
            egui::Color32::LIGHT_BLUE,
            PHASES[self.motion_capture_phase].instruction,
        );

        let mut marker = None;
        ui.horizontal(|ui| {
            if ui.button("Mark trial start").clicked() {
                marker = Some("trial_started");
            }
            if ui.button("Mark trial end").clicked() {
                marker = Some("trial_finished");
            }
            if ui.button("Next phase").clicked() {
                self.motion_capture_phase = (self.motion_capture_phase + 1).min(PHASES.len() - 1);
            }
        });

        let phase_changed = previous_phase != self.motion_capture_phase;
        let mut capture_error = None;
        if phase_changed && let Some(capture) = self.motion_capture.as_mut() {
            capture_error = capture
                .set_label(PHASES[self.motion_capture_phase].label)
                .err();
        }
        if let Some(event) = marker
            && let Some(capture) = self.motion_capture.as_mut()
        {
            capture_error = capture.mark(event).err().or(capture_error);
        }
        if let Some(error) = capture_error {
            self.log(format!("Could not write motion capture marker: {error}"));
            self.finish_motion_capture(false);
            return;
        }

        if start_experiment {
            self.begin_motion_experiment();
        } else if stop_experiment {
            self.stop_motion_experiment();
        }

        if let Some(report_id) = requested_report
            && let Some(addr) = self
                .motion_capture
                .as_ref()
                .and_then(|capture| capture.address().parse().ok())
        {
            self.send(Command::SelectSwitch2InputReport { addr, report_id });
        }

        if ui.button("Stop and save capture").clicked() {
            self.finish_motion_capture(true);
        }
    }

    fn finish_motion_capture(&mut self, restore_native_report: bool) {
        let Some(mut capture) = self.motion_capture.take() else {
            return;
        };
        let class_c_acceleration_error = self
            .class_c_acceleration_experiment
            .finish(&mut capture)
            .err();
        let rare_timing_error = self.rare_timing_experiment.finish(&mut capture).err();
        self.motion_experiment = MotionExperiment::Idle;
        self.lane_experiment.reset();
        self.class_c_acceleration_experiment.reset();
        self.class_d_experiment.reset();
        self.timing_experiment.reset();
        self.rare_timing_experiment.reset();
        self.quaternion_experiment.reset();
        if let Some(error) = class_c_acceleration_error {
            self.log(format!(
                "Could not write class-C acceleration experiment summary: {error}"
            ));
        }
        if let Some(error) = rare_timing_error {
            self.log(format!(
                "Could not write rare timing experiment summary: {error}"
            ));
        }
        if restore_native_report && let Ok(addr) = capture.address().parse() {
            self.send(Command::SelectSwitch2InputReport {
                addr,
                report_id: GAMECUBE_REPORT,
            });
        }
        let fallback_path = capture.path().to_path_buf();
        match capture.finish() {
            Ok(path) => {
                self.log(format!("Motion capture saved: {}", path.display()));
                self.last_motion_capture_path = Some(path);
            }
            Err(error) => {
                self.log(format!(
                    "Motion capture close failed for {}: {error}",
                    fallback_path.display()
                ));
                self.last_motion_capture_path = Some(fallback_path);
            }
        }
    }

    fn controller_order_entry_ui(&mut self, ui: &mut egui::Ui) {
        let available = self.br_ready || !self.ble_adapters.is_empty();
        if ui
            .add_enabled(
                available && !self.busy,
                egui::Button::new("Change controller order…").min_size(egui::vec2(210.0, 34.0)),
            )
            .clicked()
        {
            self.begin_controller_order();
        }
        if !available {
            ui.colored_label(
                egui::Color32::LIGHT_RED,
                "No Bluetooth adapter is ready for controller ordering.",
            );
        }
    }

    fn begin_controller_order(&mut self) {
        self.controller_order = true;
        self.devices.clear();
        self.selected = None;
        self.ble_devices.clear();
        self.ble_selected = None;
        self.send(Command::BeginControllerOrder {
            adapter_index: self.ble_adapter_index,
        });
    }

    fn controller_order_ui(&mut self, ui: &mut egui::Ui) {
        let mut slots: [Option<(String, ConnectedSession)>; 4] = std::array::from_fn(|_| None);
        for (addr, session) in &self.connected {
            if session.slot < slots.len() {
                slots[session.slot] = Some((addr.clone(), session.clone()));
            }
        }

        ui.add_space(14.0);
        ui.vertical_centered(|ui| {
            ui.heading("Change Controller Order");
            ui.add_space(4.0);
            ui.label("Wake or sync each controller in the player order you want.");
            ui.horizontal(|ui| {
                let br_color = if self.br_scanning {
                    egui::Color32::LIGHT_GREEN
                } else {
                    egui::Color32::GRAY
                };
                let ble_color = if self.ble_scanning {
                    egui::Color32::LIGHT_GREEN
                } else {
                    egui::Color32::GRAY
                };
                ui.colored_label(br_color, "● BR/EDR");
                ui.separator();
                ui.colored_label(ble_color, "● Switch 2 BLE");
            });
        });
        ui.add_space(16.0);

        egui::Frame::none()
            .fill(egui::Color32::from_rgb(68, 72, 76))
            .inner_margin(egui::Margin::same(18.0))
            .rounding(8.0)
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new("Press L + R, HOME, or a face button on a bonded controller")
                            .size(19.0)
                            .color(egui::Color32::WHITE),
                    );
                    ui.weak("For a new controller, hold its SYNC button; Polymer pairs recognized Nintendo devices automatically.");
                });
                ui.add_space(18.0);
                ui.columns(4, |columns| {
                    for (index, column) in columns.iter_mut().enumerate() {
                        Self::controller_slot_ui(column, index, slots[index].as_ref());
                    }
                });
            });

        ui.add_space(14.0);
        let assigned = slots.iter().flatten().count();
        ui.vertical_centered(|ui| {
            if assigned == 0 {
                ui.label("Waiting for player 1…");
            } else {
                ui.label(format!(
                    "{assigned} controller(s) assigned. Wake the next controller, or finish."
                ));
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Start over").clicked() {
                    self.begin_controller_order();
                }
                if ui
                    .add_sized([120.0, 32.0], egui::Button::new("Done"))
                    .clicked()
                {
                    self.controller_order = false;
                    self.send(Command::EndControllerOrder);
                }
            });
        });
    }

    fn controller_slot_ui(
        ui: &mut egui::Ui,
        slot: usize,
        session: Option<&(String, ConnectedSession)>,
    ) {
        let width = ui.available_width().max(120.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 220.0), egui::Sense::hover());
        let assigned = session.is_some();
        let border = if assigned {
            egui::Color32::WHITE
        } else {
            egui::Color32::from_gray(130)
        };
        ui.painter().rect(
            rect,
            5.0,
            egui::Color32::from_rgb(242, 242, 242),
            egui::Stroke::new(if assigned { 3.0_f32 } else { 1.0_f32 }, border),
        );

        let center = egui::pos2(rect.center().x, rect.top() + 92.0);
        let body = egui::Rect::from_center_size(center, egui::vec2(76.0, 42.0));
        let controller_color = if assigned {
            egui::Color32::from_rgb(68, 72, 76)
        } else {
            egui::Color32::from_gray(190)
        };
        ui.painter().rect_filled(body, 13.0, controller_color);
        ui.painter().circle_filled(
            egui::pos2(body.left() + 20.0, body.center().y),
            5.0,
            egui::Color32::from_gray(28),
        );
        for offset in [(-4.0, 0.0), (4.0, 0.0), (0.0, -4.0), (0.0, 4.0)] {
            ui.painter().circle_filled(
                egui::pos2(body.right() - 20.0 + offset.0, body.center().y + offset.1),
                2.5,
                egui::Color32::from_gray(28),
            );
        }

        ui.painter().text(
            egui::pos2(rect.center().x, rect.top() + 29.0),
            egui::Align2::CENTER_CENTER,
            format!("{}", slot + 1),
            egui::FontId::proportional(30.0),
            if assigned {
                egui::Color32::from_rgb(70, 95, 235)
            } else {
                egui::Color32::from_gray(155)
            },
        );

        if let Some((addr, session)) = session {
            let radio = match session.radio {
                RadioKind::BrEdr => "BR/EDR",
                RadioKind::Ble => "BLE",
            };
            ui.painter().text(
                egui::pos2(rect.center().x, rect.top() + 146.0),
                egui::Align2::CENTER_TOP,
                format!("{}\n{radio}\n{addr}", session.name),
                egui::FontId::proportional(13.0),
                egui::Color32::from_gray(55),
            );
        } else {
            ui.painter().text(
                egui::pos2(rect.center().x, rect.top() + 154.0),
                egui::Align2::CENTER_CENTER,
                "Waiting…",
                egui::FontId::proportional(14.0),
                egui::Color32::from_gray(120),
            );
        }
    }

    fn switch2_motion_ui(
        &mut self,
        ui: &mut egui::Ui,
        address: &str,
        controller_name: &str,
        motion: MotionFrame,
        orientation_reference: Option<[f32; 4]>,
    ) {
        ui.heading(format!("{controller_name} orientation"));
        let recenter = ui
            .button("Use current pose as face-up reference")
            .on_hover_text("Only changes the viewer reference; decoded sensor data is untouched")
            .clicked();
        let reference = if recenter {
            if let Some(view) = self.controller_inputs.get_mut(address) {
                view.orientation_reference = Some(motion.orientation);
            }
            motion.orientation
        } else {
            orientation_reference.unwrap_or(motion.orientation)
        };
        ui.horizontal_wrapped(|ui| {
            ui.monospace(format!(
                "IMU {:.2} °C · timestamp {:04X} · elapsed {} ticks",
                motion.temperature_celsius, motion.sensor_timestamp, motion.elapsed_ticks
            ));
            ui.weak("Quaternion is rendered directly; gyro is not re-integrated by the viewer.");
        });
        self.controller_model
            .show(ui, motion.orientation, reference);
    }

    fn pointer_target_window(&mut self, context: &egui::Context) {
        let mut center_clicked = false;
        let mut recalibrate_clicked = false;
        egui::Window::new("Pointer target")
            .default_pos([530.0, 70.0])
            .default_size([340.0, 410.0])
            .min_width(240.0)
            .show(context, |ui| {
                let source_text = self
                    .pointer_source
                    .map_or("Waiting for Wii data", PointerSource::label);
                ui.horizontal(|ui| {
                    ui.strong("Source");
                    let color = if self.pointer_source == Some(PointerSource::IrCamera) {
                        egui::Color32::LIGHT_GREEN
                    } else {
                        egui::Color32::LIGHT_BLUE
                    };
                    ui.colored_label(color, source_text);
                });
                ui.horizontal(|ui| {
                    center_clicked = ui.button("Center air mouse").clicked();
                    recalibrate_clicked = ui
                        .add_enabled(
                            self.last_accelerometer.is_some() && self.last_gyroscope.is_some(),
                            egui::Button::new("Recalibrate while still"),
                        )
                        .clicked();
                });
                ui.weak("Double-tap Home within 700 ms to recenter.");
                ui.add_space(6.0);

                let side = ui.available_width().clamp(180.0, 360.0);
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
                let painter = ui.painter();
                painter.rect(
                    rect,
                    4.0,
                    egui::Color32::from_rgb(18, 20, 24),
                    egui::Stroke::new(1.5_f32, egui::Color32::GRAY),
                );
                painter.line_segment(
                    [
                        egui::pos2(rect.center().x, rect.top()),
                        egui::pos2(rect.center().x, rect.bottom()),
                    ],
                    egui::Stroke::new(1.0_f32, egui::Color32::DARK_GRAY),
                );
                painter.line_segment(
                    [
                        egui::pos2(rect.left(), rect.center().y),
                        egui::pos2(rect.right(), rect.center().y),
                    ],
                    egui::Stroke::new(1.0_f32, egui::Color32::DARK_GRAY),
                );
                let target = egui::pos2(
                    rect.left() + self.pointer_position.x * rect.width(),
                    rect.top() + self.pointer_position.y * rect.height(),
                );
                painter.circle_filled(target, 10.0, egui::Color32::BLACK);
                painter.circle_stroke(
                    target,
                    10.0,
                    egui::Stroke::new(2.0_f32, egui::Color32::WHITE),
                );
                painter.circle_filled(target, 5.0, egui::Color32::LIGHT_RED);
                ui.monospace(format!(
                    "X {:.3}   Y {:.3}",
                    self.pointer_position.x, self.pointer_position.y
                ));
            });

        if center_clicked {
            self.air_mouse.center();
            if self.pointer_source != Some(PointerSource::IrCamera) {
                self.pointer_position = PointerPosition::CENTER;
                self.pointer_source = Some(PointerSource::AirMouse);
            }
            self.log("Air mouse recentered".into());
        }
        if recalibrate_clicked
            && let (Some(accelerometer), Some(gyroscope)) =
                (self.last_accelerometer, self.last_gyroscope)
        {
            self.air_mouse.recalibrate(accelerometer, gyroscope);
            if self.pointer_source != Some(PointerSource::IrCamera) {
                self.pointer_position = PointerPosition::CENTER;
                self.pointer_source = Some(PointerSource::AirMouse);
            }
            self.log("Air mouse gyro bias and level recalibrated".into());
        }
    }

    fn wii_telemetry_ui(&self, ui: &mut egui::Ui, telemetry: WiiTelemetry) {
        ui.heading(format!(
            "Wii Remote telemetry · input report 0x{:02X}",
            telemetry.report_id
        ));
        ui.horizontal_wrapped(|ui| {
            ui.strong("Buttons");
            let pressed = telemetry.pressed_buttons().collect::<Vec<_>>();
            if pressed.is_empty() {
                ui.weak("None");
            } else {
                for name in pressed {
                    ui.colored_label(egui::Color32::LIGHT_GREEN, format!("[{name}]"));
                }
            }
        });
        ui.add_space(4.0);
        ui.columns(3, |columns| {
            columns[0].strong("Accelerometer · raw 10-bit");
            if let Some(accel) = telemetry.accelerometer {
                columns[0].monospace(format!(
                    "X {:4}   Y {:4}   Z {:4}",
                    accel.x, accel.y, accel.z
                ));
            } else {
                columns[0].weak("Not present in this report");
            }

            columns[1].strong("MotionPlus · raw 14-bit");
            if let Some(gyro) = telemetry.gyroscope {
                columns[1].monospace(format!(
                    "Pitch {:5}  Δ{:+5}  {}\nRoll  {:5}  Δ{:+5}  {}\nYaw   {:5}  Δ{:+5}  {}",
                    gyro.pitch,
                    i32::from(gyro.pitch) - 8192,
                    gyro_range(gyro.pitch_slow),
                    gyro.roll,
                    i32::from(gyro.roll) - 8192,
                    gyro_range(gyro.roll_slow),
                    gyro.yaw,
                    i32::from(gyro.yaw) - 8192,
                    gyro_range(gyro.yaw_slow),
                ));
            } else {
                columns[1].weak("Waiting for MotionPlus data");
            }

            columns[2].strong("IR camera · 1024 × 768");
            if let Some(points) = telemetry.ir_points {
                for (index, point) in points.into_iter().enumerate() {
                    if point.visible {
                        columns[2].monospace(format!(
                            "Point {}  X {:4}  Y {:3}",
                            index + 1,
                            point.x,
                            point.y
                        ));
                    } else {
                        columns[2].weak(format!("Point {}  not visible", index + 1));
                    }
                }
            } else {
                columns[2].weak("Not present in this report");
            }
        });
    }

    fn discovery_ui(&mut self, ui: &mut egui::Ui) {
        match self.radio {
            RadioKind::BrEdr => self.bredr_discovery_ui(ui),
            RadioKind::Ble => self.ble_discovery_ui(ui),
        }
    }

    fn bredr_discovery_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("BR/EDR discovery");
        self.controller_order_entry_ui(ui);
        ui.label("The order screen listens for bonded controller wake-ups and fresh Nintendo pairing on both Bluetooth radios.");
        ui.add_space(6.0);
        egui::ScrollArea::vertical()
            .max_height(330.0)
            .show(ui, |ui| {
                for (index, device) in self.devices.iter().enumerate() {
                    let title = format!(
                        "{}\n{} · RSSI {} · class {}",
                        device.profile_name.unwrap_or(&device.name),
                        device.addr,
                        device.rssi.map_or("?".into(), |v| format!("{v} dBm")),
                        device.class
                    );
                    if ui
                        .selectable_label(self.selected == Some(index), title)
                        .clicked()
                    {
                        self.selected = Some(index);
                        if device.is_wii_remote() && self.pin == BondingMode::Ssp {
                            self.pin = BondingMode::WiiSync;
                        } else if device.is_switch() {
                            self.pin = BondingMode::Ssp;
                        }
                    }
                }
            });
    }

    fn ble_discovery_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Switch 2 BLE discovery");
        if self.ble_adapters.is_empty() {
            ui.colored_label(egui::Color32::LIGHT_RED, "No OS BLE adapter available");
        } else {
            egui::ComboBox::from_label("OS-owned adapter")
                .selected_text(
                    self.ble_adapters
                        .get(self.ble_adapter_index)
                        .map_or("Unknown", String::as_str),
                )
                .show_ui(ui, |ui| {
                    for (index, adapter) in self.ble_adapters.iter().enumerate() {
                        ui.selectable_value(&mut self.ble_adapter_index, index, adapter);
                    }
                });
        }
        self.controller_order_entry_ui(ui);
        ui.label("Bonded controllers reconnect by button wake. A controller in SYNC mode is paired automatically when its Nintendo GATT service appears.");
        ui.add_space(6.0);
        egui::ScrollArea::vertical()
            .max_height(330.0)
            .show(ui, |ui| {
                for (index, device) in self.ble_devices.iter().enumerate() {
                    let title = format!(
                        "{}\n{} · RSSI {}",
                        device
                            .name
                            .as_deref()
                            .unwrap_or("Unknown Switch 2 controller"),
                        device.addr,
                        device.rssi.map_or("?".into(), |v| format!("{v} dBm")),
                    );
                    if ui
                        .selectable_label(self.ble_selected == Some(index), title)
                        .clicked()
                    {
                        self.ble_selected = Some(index);
                    }
                }
            });
    }

    fn connection_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading(format!("Connected sessions · {}", self.connected.len()));
        if self.connected.is_empty() {
            ui.weak("None");
        } else {
            for (addr, session) in &self.connected {
                let label = format!(
                    "P{} · {} · {addr} · {}",
                    session.slot + 1,
                    session.name,
                    match session.radio {
                        RadioKind::BrEdr => "BR/EDR",
                        RadioKind::Ble => "BLE",
                    }
                );
                ui.selectable_value(&mut self.selected_connection, Some(addr.clone()), label);
            }
        }
        ui.separator();
        match self.radio {
            RadioKind::BrEdr => self.bredr_connection_ui(ui),
            RadioKind::Ble => self.ble_connection_ui(ui),
        }
    }

    fn bredr_connection_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Sync and HID");
        egui::ComboBox::from_label("Pairing method")
            .selected_text(self.pin.label())
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.pin, BondingMode::Ssp, BondingMode::Ssp.label());
                ui.selectable_value(
                    &mut self.pin,
                    BondingMode::WiiOneTwo,
                    BondingMode::WiiOneTwo.label(),
                );
                ui.selectable_value(
                    &mut self.pin,
                    BondingMode::WiiSync,
                    BondingMode::WiiSync.label(),
                );
            });
        ui.label(self.pin.help());
        if self
            .selected
            .and_then(|index| self.devices.get(index))
            .is_some_and(DeviceView::is_wii_remote)
            && self.pin == BondingMode::Ssp
        {
            ui.colored_label(
                egui::Color32::LIGHT_RED,
                "A Wii Remote requires one of the two legacy binary PIN modes.",
            );
        }

        let connect_clicked = ui
            .add_enabled(
                self.br_ready && !self.busy && self.selected.is_some(),
                egui::Button::new("Pair / connect selected"),
            )
            .clicked();
        if let (true, Some(device)) = (
            connect_clicked,
            self.selected
                .and_then(|index| self.devices.get(index))
                .cloned(),
        ) {
            self.send(Command::ConnectBr {
                device: device.inquiry,
                pin: self.pin,
                profile_id: device.profile_id,
            });
        }
        let forget_clicked = ui
            .add_enabled(
                self.br_ready && !self.busy && self.selected.is_some(),
                egui::Button::new("Forget selected link key"),
            )
            .clicked();
        if let (true, Some(addr)) = (
            forget_clicked,
            self.selected
                .and_then(|index| self.devices.get(index))
                .map(|device| device.inquiry.addr),
        ) {
            self.send(Command::Forget(addr));
        }

        let mut listening = self.listening;
        if ui
            .checkbox(&mut listening, "Accept inbound reconnects")
            .changed()
        {
            self.listening = listening;
            self.send(Command::ListenInbound(listening));
        }
        ui.label("Known bonded Wii, Switch 1, and NSO controllers can reconnect by button press. For the SNES controller, press L+R.");

        if ui
            .add_enabled(
                self.selected_connection.is_some() && !self.busy,
                egui::Button::new("Disconnect selected session"),
            )
            .clicked()
            && let Some(addr) = self.selected_connection_addr()
        {
            self.send(Command::Disconnect(Some(addr)));
        }
        if ui
            .add_enabled(
                self.connected.len() > 1 && !self.busy,
                egui::Button::new("Disconnect all sessions"),
            )
            .clicked()
        {
            self.send(Command::Disconnect(None));
        }

        ui.separator();
        ui.label("Output report (hex; HIDP 0xA2 is added automatically)");
        ui.text_edit_singleline(&mut self.output_report);
        if ui
            .add_enabled(
                self.selected_connection.is_some(),
                egui::Button::new("Send output report"),
            )
            .clicked()
        {
            match (
                self.selected_connection_addr(),
                parse_hex(&self.output_report),
            ) {
                (Some(addr), Ok(bytes)) => self.send(Command::SendOutput { addr, bytes }),
                (None, _) => self.log("Select a connected session first".into()),
                (_, Err(error)) => self.log(error),
            }
        }
    }

    fn ble_connection_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Switch 2 GATT");
        let connect_clicked = ui
            .add_enabled(
                !self.busy && !self.ble_adapters.is_empty() && self.ble_selected.is_some(),
                egui::Button::new("Connect selected over BLE"),
            )
            .clicked();
        if let (true, Some(device)) = (
            connect_clicked,
            self.ble_selected
                .and_then(|index| self.ble_devices.get(index))
                .cloned(),
        ) {
            self.send(Command::ConnectBle {
                adapter_index: self.ble_adapter_index,
                device,
            });
        }

        ui.label("Connection subscribes to the model-specific report (including NSO GameCube report 0x0A), sends calibration initialization immediately, and enables the full feature mask.");
        ui.colored_label(
            egui::Color32::YELLOW,
            "Arbitrary BLE writes are disabled: malformed pairing commands can make a controller unusable.",
        );

        let selected_ble = self
            .selected_connection
            .as_ref()
            .and_then(|addr| self.connected.get(addr))
            .is_some_and(|session| session.radio == RadioKind::Ble);
        if ui
            .add_enabled(
                selected_ble && !self.busy,
                egui::Button::new("Sleep selected controller (keep wake armed)"),
            )
            .clicked()
            && let Some(addr) = self.selected_connection_addr()
        {
            self.send(Command::SleepBle(addr));
        }
        ui.weak("Use Sleep to test bonded wake. After its LEDs turn off, press HOME or another controller button; do not press SYNC.");

        if ui
            .add_enabled(
                self.selected_connection.is_some() && !self.busy,
                egui::Button::new("Disconnect selected session"),
            )
            .clicked()
            && let Some(addr) = self.selected_connection_addr()
        {
            self.send(Command::Disconnect(Some(addr)));
        }
    }

    fn selected_connection_addr(&self) -> Option<DeviceAddr> {
        self.selected_connection.as_deref()?.parse().ok()
    }
}

impl Drop for BluetoothTestApp {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
    }
}

fn gyro_range(slow: bool) -> &'static str {
    if slow { "slow" } else { "fast" }
}

fn ir_pointer_position(telemetry: WiiTelemetry) -> Option<PointerPosition> {
    let points = telemetry.ir_points?;
    let (x_sum, y_sum, count) = points
        .into_iter()
        .filter(|point| point.visible)
        .fold((0u32, 0u32, 0u32), |(x, y, count), point| {
            (x + u32::from(point.x), y + u32::from(point.y), count + 1)
        });
    (count != 0).then(|| {
        PointerPosition {
            // The camera observes the scene from the controller, so its X
            // coordinate runs opposite to screen-space X.
            x: 1.0 - x_sum as f32 / count as f32 / 1023.0,
            y: y_sum as f32 / count as f32 / 767.0,
        }
        .clamped()
    })
}

fn register_home_press(previous: &mut Option<Instant>, pressed_at: Instant) -> bool {
    if previous.is_some_and(|at| pressed_at.saturating_duration_since(at) <= DOUBLE_HOME_WINDOW) {
        *previous = None;
        true
    } else {
        *previous = Some(pressed_at);
        false
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_hex(value: &str) -> Result<Vec<u8>, String> {
    let compact: String = value
        .chars()
        .filter(|c| !c.is_ascii_whitespace() && *c != ':' && *c != '-')
        .collect();
    if !compact.len().is_multiple_of(2) {
        return Err("Hex report must contain complete byte pairs".into());
    }
    (0..compact.len())
        .step_by(2)
        .map(|at| {
            u8::from_str_radix(&compact[at..at + 2], 16)
                .map_err(|_| format!("Invalid hex near byte {}", at / 2))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::wiimote::IrPoint;

    #[test]
    fn accepts_spaced_hex_reports() {
        assert_eq!(parse_hex("01 A2:ff-08").unwrap(), [1, 0xa2, 0xff, 8]);
    }

    #[test]
    fn rejects_half_bytes() {
        assert!(parse_hex("abc").is_err());
    }

    #[test]
    fn averages_only_visible_ir_points() {
        let telemetry = WiiTelemetry {
            report_id: 0x37,
            buttons: 0,
            accelerometer: None,
            gyroscope: None,
            ir_points: Some([
                IrPoint {
                    x: 0,
                    y: 0,
                    visible: true,
                },
                IrPoint {
                    x: 1023,
                    y: 767,
                    visible: true,
                },
                IrPoint {
                    x: 1023,
                    y: 1023,
                    visible: false,
                },
                IrPoint {
                    x: 1023,
                    y: 1023,
                    visible: false,
                },
            ]),
        };
        let position = ir_pointer_position(telemetry).unwrap();
        assert!((position.x - 0.5).abs() < 0.001);
        assert!((position.y - 0.5).abs() < 0.001);
    }

    #[test]
    fn mirrors_ir_x_into_screen_space() {
        let telemetry = WiiTelemetry {
            report_id: 0x37,
            buttons: 0,
            accelerometer: None,
            gyroscope: None,
            ir_points: Some([
                IrPoint {
                    x: 0,
                    y: 384,
                    visible: true,
                },
                IrPoint {
                    x: 1023,
                    y: 1023,
                    visible: false,
                },
                IrPoint {
                    x: 1023,
                    y: 1023,
                    visible: false,
                },
                IrPoint {
                    x: 1023,
                    y: 1023,
                    visible: false,
                },
            ]),
        };
        assert_eq!(ir_pointer_position(telemetry).unwrap().x, 1.0);
    }

    #[test]
    fn double_home_requires_two_presses_inside_window() {
        let start = Instant::now();
        let mut previous = None;
        assert!(!register_home_press(&mut previous, start));
        assert!(register_home_press(
            &mut previous,
            start + Duration::from_millis(650)
        ));
        assert_eq!(previous, None);
        assert!(!register_home_press(
            &mut previous,
            start + Duration::from_secs(2)
        ));
        assert!(!register_home_press(
            &mut previous,
            start + Duration::from_secs(3)
        ));
    }
}
