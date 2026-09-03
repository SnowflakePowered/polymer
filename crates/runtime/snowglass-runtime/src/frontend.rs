//! Application-facing services used by the Snowglass Deno extension.
//!
//! Domain policy lives here. The shell supplies only thread-affine platform
//! mechanics such as presentation, input routing, and window attachment.

use crate::ConfigurationDefaults;
use parking_lot::{Mutex, RwLock};
use serde_json::Value;
use snowglass_controllers::ControllerProfiles;
use snowglass_deno::snowglass::{
    ApiFuture, ConsoleStorageRequest, InstanceToken, SnowglassHost, SnowglassNotificationSender,
};
use snowglass_input::{
    Calibration, CalibrationCapture, CalibrationKind, DeviceId, Feedback, InputEvent, InputHub,
    InputListener, IrMode, IrSensitivity, ListenerHandle, MotionCalibration, PointerSetup,
    SensorBarPosition, StickCalibration, TriggerCalibration,
};
use snowglass_library::StorageManager;
use snowglass_pluginhost::EmulatorPluginInstance;
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

const CONTROLLER_API_RESPONSE_ATTEMPTS: usize = 3;
const CONTROLLER_API_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
const CONTROLLER_CHANGE_RESPONSE_ATTEMPTS: usize =
    snowglass_controllers::DEFAULT_CONTROLLER_TOPOLOGY_TRANSACTION_TIMEOUT.as_secs() as usize
        / CONTROLLER_API_RESPONSE_TIMEOUT.as_secs() as usize
        + 1;

/// Thread-affine mechanics retained by the native shell.
///
/// Implementations must not interpret JavaScript APIs; they receive validated
/// native requests from [`FrontendServices`].
pub trait RuntimePlatform: Send + Sync {
    fn emulator_launch(
        &self,
        token: InstanceToken,
        options: Value,
        prepared_filesystem: Option<snowglass_vfs::PreparedFilesystemLaunch>,
        controller_session: Option<snowglass_pluginhost::ControllerSession>,
    ) -> ApiFuture<Result<(), String>>;
    fn emulator_close(&self, token: InstanceToken);
    fn emulator_set_volume(&self, token: InstanceToken, volume: f64);
    fn emulator_set_muted(&self, token: InstanceToken, muted: bool);
    fn emulator_message(&self, token: InstanceToken, name: String, args: Value);
    fn emulator_input(&self, token: InstanceToken, active: bool);
    fn emulator_pause_state(
        &self,
        token: InstanceToken,
        pause: bool,
        timeout: Duration,
    ) -> ApiFuture<Result<(), String>>;
    fn emulator_save_state(
        &self,
        token: InstanceToken,
        timeout: Duration,
    ) -> ApiFuture<Result<Value, String>>;
    fn emulator_resume_state(
        &self,
        token: InstanceToken,
        slot: u32,
        timeout: Duration,
    ) -> ApiFuture<Result<Value, String>>;
    fn emulator_state_screenshot(
        &self,
        token: InstanceToken,
        record_id: String,
    ) -> ApiFuture<Result<Option<Vec<u8>>, String>>;
    fn input_configure(&self, request_id: u32, transaction: Value);
    fn controller_api_snapshot(&self) -> Value;
    fn controller_api_command(&self, command: Value) -> ApiFuture<Result<Value, String>>;
    fn webrtc_start(
        &self,
        token: InstanceToken,
        bind: SocketAddr,
        targets: Vec<u32>,
    ) -> ApiFuture<Result<(), String>>;
    fn webrtc_stop(&self, token: InstanceToken) -> ApiFuture<Result<(), String>>;
}

#[derive(Debug)]
pub enum EmulatorCommand {
    Launch {
        token: InstanceToken,
        options: Value,
        prepared_filesystem: Option<snowglass_vfs::PreparedFilesystemLaunch>,
        controller_session: Option<snowglass_pluginhost::ControllerSession>,
        result: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    Close(InstanceToken),
    SetVolume {
        token: InstanceToken,
        volume: f64,
    },
    SetMuted {
        token: InstanceToken,
        muted: bool,
    },
    Message {
        token: InstanceToken,
        name: String,
        args: Value,
    },
    Input {
        token: InstanceToken,
        active: bool,
    },
    PauseState {
        token: InstanceToken,
        pause: bool,
        timeout: Duration,
        result: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    SaveState {
        token: InstanceToken,
        timeout: Duration,
        result: tokio::sync::oneshot::Sender<Result<Value, String>>,
    },
    ResumeState {
        token: InstanceToken,
        slot: u32,
        timeout: Duration,
        result: tokio::sync::oneshot::Sender<Result<Value, String>>,
    },
    StateScreenshot {
        token: InstanceToken,
        record_id: String,
        result: tokio::sync::oneshot::Sender<Result<Option<Vec<u8>>, String>>,
    },
}

#[derive(Debug)]
pub enum InputCommand {
    Configure {
        request_id: u32,
        transaction: Value,
    },
    ControllerApi {
        command: Value,
        result: tokio::sync::oneshot::Sender<Result<Value, String>>,
    },
}

#[derive(Clone)]
pub struct ControllerApiState {
    state: Arc<RwLock<Value>>,
    controller_profiles: ControllerProfiles,
    controller_assignments: Arc<RwLock<snowglass_controllers::ControllerAssignmentSnapshot>>,
}

impl Default for ControllerApiState {
    fn default() -> Self {
        Self {
            state: Arc::new(RwLock::new(serde_json::json!({
                "revision": 0,
                "controllers": {
                    "availability": { "osBle": "unknown", "winUsb": "unknown" },
                    "devices": [],
                    "players": [
                        { "index": 0, "controller": null },
                        { "index": 1, "controller": null },
                        { "index": 2, "controller": null },
                        { "index": 3, "controller": null }
                    ],
                    "activeConnectionSession": null,
                    "layouts": []
                }
            }))),
            controller_profiles: ControllerProfiles::default(),
            controller_assignments: Arc::new(RwLock::new(Default::default())),
        }
    }
}

impl ControllerApiState {
    pub fn snapshot(&self) -> Value {
        self.state.read().clone()
    }

    pub fn controller_profiles(&self) -> ControllerProfiles {
        self.controller_profiles.clone()
    }

    pub(crate) fn controller_assignments(
        &self,
    ) -> snowglass_controllers::ControllerAssignmentSnapshot {
        self.controller_assignments.read().clone()
    }

    pub fn publish(
        &self,
        controllers: Value,
        assignments: snowglass_controllers::ControllerAssignmentSnapshot,
    ) {
        let mut state = self.state.write();
        if state["controllers"] == controllers {
            return;
        }
        *self.controller_assignments.write() = assignments;
        let revision = state["revision"]
            .as_u64()
            .unwrap_or_default()
            .wrapping_add(1);
        *state = serde_json::json!({
            "revision": revision,
            "controllers": controllers,
        });
    }
}

#[derive(Debug)]
pub enum WebRtcCommand {
    Start {
        token: InstanceToken,
        bind: SocketAddr,
        targets: Vec<u32>,
        result: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    Stop {
        token: InstanceToken,
        result: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
}

pub struct CommandSink<T>(Arc<dyn Fn(T) + Send + Sync>);

impl<T> Clone for CommandSink<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> CommandSink<T> {
    pub fn new(send: impl Fn(T) + Send + Sync + 'static) -> Self {
        Self(Arc::new(send))
    }

    pub fn send(&self, command: T) {
        (self.0)(command);
    }
}

impl<T> Default for CommandSink<T> {
    fn default() -> Self {
        Self::new(|_| {})
    }
}

pub type EmulatorCommandSink = CommandSink<EmulatorCommand>;
pub type InputCommandSink = CommandSink<InputCommand>;
pub type WebRtcCommandSink = CommandSink<WebRtcCommand>;

/// Transitional adapter for hosts whose platform state is confined to an
/// existing native event loop. Snowglass ops still target `RuntimePlatform`;
/// the event-loop transport is a runtime implementation detail.
pub struct CommandRuntimePlatform {
    emulator: EmulatorCommandSink,
    input: InputCommandSink,
    controller_api: ControllerApiState,
    webrtc: WebRtcCommandSink,
}

impl CommandRuntimePlatform {
    pub fn new(
        emulator: EmulatorCommandSink,
        input: InputCommandSink,
        controller_api: ControllerApiState,
        webrtc: WebRtcCommandSink,
    ) -> Self {
        Self {
            emulator,
            input,
            controller_api,
            webrtc,
        }
    }
}

impl RuntimePlatform for CommandRuntimePlatform {
    fn emulator_launch(
        &self,
        token: InstanceToken,
        options: Value,
        prepared_filesystem: Option<snowglass_vfs::PreparedFilesystemLaunch>,
        controller_session: Option<snowglass_pluginhost::ControllerSession>,
    ) -> ApiFuture<Result<(), String>> {
        let sink = self.emulator.clone();
        Box::pin(async move {
            let (result, response) = tokio::sync::oneshot::channel();
            sink.send(EmulatorCommand::Launch {
                token,
                options,
                prepared_filesystem,
                controller_session,
                result,
            });
            response
                .await
                .map_err(|_| "emulator host is unavailable".to_owned())?
        })
    }

    fn emulator_close(&self, token: InstanceToken) {
        self.emulator.send(EmulatorCommand::Close(token));
    }

    fn emulator_set_volume(&self, token: InstanceToken, volume: f64) {
        self.emulator
            .send(EmulatorCommand::SetVolume { token, volume });
    }

    fn emulator_set_muted(&self, token: InstanceToken, muted: bool) {
        self.emulator
            .send(EmulatorCommand::SetMuted { token, muted });
    }

    fn emulator_message(&self, token: InstanceToken, name: String, args: Value) {
        self.emulator
            .send(EmulatorCommand::Message { token, name, args });
    }

    fn emulator_input(&self, token: InstanceToken, active: bool) {
        self.emulator.send(EmulatorCommand::Input { token, active });
    }

    fn emulator_pause_state(
        &self,
        token: InstanceToken,
        pause: bool,
        timeout: Duration,
    ) -> ApiFuture<Result<(), String>> {
        let sink = self.emulator.clone();
        Box::pin(async move {
            let (result, response) = tokio::sync::oneshot::channel();
            sink.send(EmulatorCommand::PauseState {
                token,
                pause,
                timeout,
                result,
            });
            response
                .await
                .map_err(|_| "emulator host is unavailable".to_owned())?
        })
    }

    fn emulator_save_state(
        &self,
        token: InstanceToken,
        timeout: Duration,
    ) -> ApiFuture<Result<Value, String>> {
        let sink = self.emulator.clone();
        Box::pin(async move {
            let (result, response) = tokio::sync::oneshot::channel();
            sink.send(EmulatorCommand::SaveState {
                token,
                timeout,
                result,
            });
            response
                .await
                .map_err(|_| "emulator host is unavailable".to_owned())?
        })
    }

    fn emulator_resume_state(
        &self,
        token: InstanceToken,
        slot: u32,
        timeout: Duration,
    ) -> ApiFuture<Result<Value, String>> {
        let sink = self.emulator.clone();
        Box::pin(async move {
            let (result, response) = tokio::sync::oneshot::channel();
            sink.send(EmulatorCommand::ResumeState {
                token,
                slot,
                timeout,
                result,
            });
            response
                .await
                .map_err(|_| "emulator host is unavailable".to_owned())?
        })
    }

    fn emulator_state_screenshot(
        &self,
        token: InstanceToken,
        record_id: String,
    ) -> ApiFuture<Result<Option<Vec<u8>>, String>> {
        let sink = self.emulator.clone();
        Box::pin(async move {
            let (result, response) = tokio::sync::oneshot::channel();
            sink.send(EmulatorCommand::StateScreenshot {
                token,
                record_id,
                result,
            });
            response
                .await
                .map_err(|_| "emulator host is unavailable".to_owned())?
        })
    }

    fn input_configure(&self, request_id: u32, transaction: Value) {
        self.input.send(InputCommand::Configure {
            request_id,
            transaction,
        });
    }

    fn controller_api_snapshot(&self) -> Value {
        self.controller_api.snapshot()
    }

    fn controller_api_command(&self, command: Value) -> ApiFuture<Result<Value, String>> {
        let sink = self.input.clone();
        Box::pin(async move {
            let (result, mut response) = tokio::sync::oneshot::channel();
            let response_attempts =
                if command["type"] == snowglass_controllers::CONTROLLER_SESSION_COMMIT_COMMAND {
                    // Live emulator reconfiguration can include several UIA
                    // operations. Give it enough time to reach the plugin's
                    // 60-second deadline plus one response interval.
                    CONTROLLER_CHANGE_RESPONSE_ATTEMPTS
                } else {
                    CONTROLLER_API_RESPONSE_ATTEMPTS
                };
            sink.send(InputCommand::ControllerApi { command, result });
            for attempt in 1..=response_attempts {
                match tokio::time::timeout(CONTROLLER_API_RESPONSE_TIMEOUT, &mut response).await {
                    Ok(result) => {
                        return result.map_err(|_| "controller host is unavailable".to_owned())?;
                    }
                    Err(_) if attempt < response_attempts => {
                        tracing::warn!(
                            attempt,
                            "controller API response is delayed; continuing to wait"
                        );
                    }
                    Err(_) => return Err("controller API command timed out".to_owned()),
                }
            }
            unreachable!("controller API response attempts are nonzero")
        })
    }

    fn webrtc_start(
        &self,
        token: InstanceToken,
        bind: SocketAddr,
        targets: Vec<u32>,
    ) -> ApiFuture<Result<(), String>> {
        let sink = self.webrtc.clone();
        Box::pin(async move {
            let (result, response) = tokio::sync::oneshot::channel();
            sink.send(WebRtcCommand::Start {
                token,
                bind,
                targets,
                result,
            });
            response
                .await
                .map_err(|_| "WebRTC host is unavailable".to_owned())?
        })
    }

    fn webrtc_stop(&self, token: InstanceToken) -> ApiFuture<Result<(), String>> {
        let sink = self.webrtc.clone();
        Box::pin(async move {
            let (result, response) = tokio::sync::oneshot::channel();
            sink.send(WebRtcCommand::Stop { token, result });
            response
                .await
                .map_err(|_| "WebRTC host is unavailable".to_owned())?
        })
    }
}

#[derive(Clone, Default)]
struct PreparedFilesystemStore(Arc<PreparedFilesystemStoreInner>);

#[derive(Default)]
struct PreparedFilesystemStoreInner {
    next: AtomicU64,
    filesystems: Mutex<HashMap<u64, snowglass_vfs::PreparedFilesystemLaunch>>,
}

#[derive(Clone, Default)]
struct ControllerSessionStore(Arc<ControllerSessionStoreInner>);

#[derive(Default)]
struct ControllerSessionStoreInner {
    next: AtomicU64,
    sessions: Mutex<HashMap<u64, snowglass_pluginhost::ControllerSession>>,
}

impl ControllerSessionStore {
    fn insert(&self, session: snowglass_pluginhost::ControllerSession) -> String {
        let token = self.0.next.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        self.0.sessions.lock().insert(token, session);
        format!("{token:016x}")
    }

    fn take(&self, token: &str) -> Option<snowglass_pluginhost::ControllerSession> {
        let token = u64::from_str_radix(token, 16).ok()?;
        self.0.sessions.lock().remove(&token)
    }
}

impl PreparedFilesystemStore {
    fn insert(&self, filesystem: snowglass_vfs::PreparedFilesystemLaunch) -> String {
        let token = self.0.next.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        self.0.filesystems.lock().insert(token, filesystem);
        format!("{token:016x}")
    }

    fn take(&self, token: &str) -> Option<snowglass_vfs::PreparedFilesystemLaunch> {
        let token = u64::from_str_radix(token, 16).ok()?;
        self.0.filesystems.lock().remove(&token)
    }

    fn release(&self, token: &str) -> bool {
        self.take(token).is_some()
    }
}

/// Runtime-owned implementation of all non-DOM `Snowglass.*` services.
pub struct FrontendServices {
    plugins: Arc<snowglass_pluginhost::PluginRuntime>,
    configuration: Option<ConfigurationDefaults>,
    storage: Option<Arc<StorageManager>>,
    prepared_filesystems: PreparedFilesystemStore,
    controller_sessions: ControllerSessionStore,
    input_sources: InputHub,
    controller_api: ControllerApiState,
    platform: Arc<dyn RuntimePlatform>,
    notifications: SnowglassNotificationSender,
    deno_input: Arc<DenoInputListener>,
    input_listener: ListenerHandle,
    calibration_captures: Mutex<HashMap<DeviceId, CalibrationCapture>>,
}

struct DenoInputListener {
    hub: InputHub,
    notifications: SnowglassNotificationSender,
    dirty: [AtomicBool; 8],
}

impl DenoInputListener {
    fn new(hub: InputHub, notifications: SnowglassNotificationSender) -> Self {
        Self {
            hub,
            notifications,
            dirty: std::array::from_fn(|_| AtomicBool::new(false)),
        }
    }

    fn get(&self, index: u8) -> Option<Value> {
        let index = snowglass_input::PlayerIndex::new(index);
        // Rearm notifications before taking the snapshot. A grip/order session
        // temporarily unassigns players, so the queued read may have no state;
        // leaving the bit set in that case suppresses every later input event.
        // Clearing first also prevents a state update between the snapshot and
        // acknowledgement from being mistaken for the notification we consumed.
        self.dirty[usize::from(index.get())].store(false, Ordering::Release);
        let state = self.hub.state(index)?;
        Some(serde_json::json!({
            "buttons": state.gamepad.buttons, "leftStick": state.gamepad.left_stick,
            "rightStick": state.gamepad.right_stick, "leftTrigger": state.gamepad.left_trigger,
            "rightTrigger": state.gamepad.right_trigger, "accelerometer": state.sensor.accelerometer,
            "gyroscope": state.sensor.gyroscope, "orientation": state.sensor.has_orientation.then_some(state.sensor.orientation),
            "pointer": { "position": state.pointer.position, "delta": state.pointer.delta, "flags": state.pointer.flags.bits(), "kind": format!("{:?}", state.pointer.kind) },
            "battery": { "level": state.battery.level, "charging": state.battery.charging }
        }))
    }
}

impl InputListener for DenoInputListener {
    fn on_input(&self, event: InputEvent<'_>) {
        use snowglass_deno::snowglass::SnowglassNotification;
        let InputEvent::State { index, .. } = event else {
            return;
        };
        if self.dirty[usize::from(index.get())].swap(true, Ordering::AcqRel) {
            return;
        }
        let _ = self
            .notifications
            .send(SnowglassNotification::InputChanged { index: index.get() });
    }
}

struct ControllerDeviceCommandDecoder(Value);

impl ControllerDeviceCommandDecoder {
    fn new(value: Value) -> Self {
        Self(value)
    }

    fn execute(
        &self,
        hub: &InputHub,
        listener: &ListenerHandle,
        captures: &Mutex<HashMap<DeviceId, CalibrationCapture>>,
    ) -> Result<Value, String> {
        match self.string("type")? {
            "rumble" => {
                listener
                    .feedback(
                        self.index()?,
                        Feedback::Rumble {
                            low_frequency: self.u16("lowFrequency")?,
                            high_frequency: self.u16("highFrequency")?,
                            duration_ms: self.u32("durationMs")?,
                        },
                    )
                    .map_err(|error| error.to_string())?;
                Ok(Value::Null)
            }
            "recenter" => {
                listener
                    .feedback(self.index()?, Feedback::Recenter)
                    .map_err(|error| error.to_string())?;
                Ok(Value::Null)
            }
            "getCalibration" => Ok(Self::calibration_value(
                &hub.calibration(&self.device(hub)?),
            )),
            "calibrate" => {
                let device = self.device(hub)?;
                let calibration = Self::calibration(
                    self.0
                        .get("calibration")
                        .ok_or_else(|| "calibration is required".to_owned())?,
                )?;
                hub.calibrate(&device, &calibration)
                    .map_err(|error| error.to_string())?;
                Ok(Self::calibration_value(&hub.calibration(&device)))
            }
            "resetCalibration" => {
                let device = self.device(hub)?;
                hub.reset_calibration(&device)
                    .map_err(|error| error.to_string())?;
                Ok(Self::calibration_value(&hub.calibration(&device)))
            }
            "beginCalibration" => {
                let device = self.device(hub)?;
                let kind = match self.string("kind")? {
                    "stickCenter" => CalibrationKind::StickCenter,
                    "stickRange" => CalibrationKind::StickRange,
                    "triggerRange" => CalibrationKind::TriggerRange,
                    "motionBias" => CalibrationKind::MotionBias,
                    _ => return Err("unknown calibration kind".to_owned()),
                };
                let capture = hub
                    .begin_calibration(&device, kind)
                    .map_err(|error| error.to_string())?;
                captures.lock().insert(device, capture);
                Ok(serde_json::json!({ "progress": 0.0 }))
            }
            "calibrationProgress" => {
                let device = self.device(hub)?;
                Ok(
                    serde_json::json!({ "progress": captures.lock().get(&device).map_or(0.0, CalibrationCapture::progress) }),
                )
            }
            "finishCalibration" => {
                let device = self.device(hub)?;
                let capture = captures
                    .lock()
                    .remove(&device)
                    .ok_or_else(|| "calibration capture is not active".to_owned())?;
                capture.finish().map_err(|error| error.to_string())?;
                Ok(Self::calibration_value(&hub.calibration(&device)))
            }
            "cancelCalibration" => {
                let device = self.device(hub)?;
                if let Some(capture) = captures.lock().remove(&device) {
                    capture.cancel();
                }
                Ok(Value::Null)
            }
            _ => Err("unknown input command".to_owned()),
        }
    }

    fn index(&self) -> Result<snowglass_input::PlayerIndex, String> {
        Ok(snowglass_input::PlayerIndex::new(self.u8("index")?))
    }

    fn device(&self, hub: &InputHub) -> Result<DeviceId, String> {
        let id = self.string("deviceId")?;
        hub.devices()
            .into_iter()
            .map(|(_, device)| device.id)
            .find(|device| device.as_str() == id)
            .ok_or_else(|| format!("unknown input device {id}"))
    }

    fn string(&self, name: &str) -> Result<&str, String> {
        self.0
            .get(name)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{name} must be a string"))
    }

    fn u8(&self, name: &str) -> Result<u8, String> {
        u8::try_from(self.u64(name)?).map_err(|_| format!("{name} is out of range"))
    }
    fn u16(&self, name: &str) -> Result<u16, String> {
        u16::try_from(self.u64(name)?).map_err(|_| format!("{name} is out of range"))
    }
    fn u32(&self, name: &str) -> Result<u32, String> {
        u32::try_from(self.u64(name)?).map_err(|_| format!("{name} is out of range"))
    }
    fn u64(&self, name: &str) -> Result<u64, String> {
        self.0
            .get(name)
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("{name} must be an unsigned integer"))
    }

    fn calibration(value: &Value) -> Result<Calibration, String> {
        let mut calibration = Calibration::default();
        if let Some(sticks) = value.get("sticks").and_then(Value::as_array) {
            for (index, stick) in sticks.iter().take(2).enumerate() {
                if !stick.is_null() {
                    calibration.sticks[index] = Some(Self::stick(stick)?);
                }
            }
        }
        if let Some(triggers) = value.get("triggers").and_then(Value::as_array) {
            for (index, trigger) in triggers.iter().take(2).enumerate() {
                if !trigger.is_null() {
                    calibration.triggers[index] = Some(Self::trigger(trigger)?);
                }
            }
        }
        if let Some(motion) = value.get("motion").filter(|value| !value.is_null()) {
            calibration.motion = Some(MotionCalibration {
                accelerometer_bias: Self::array(motion, "accelerometerBias")?,
                accelerometer_scale: Self::array(motion, "accelerometerScale")?,
                gyroscope_bias: Self::array(motion, "gyroscopeBias")?,
                gyroscope_scale: Self::array(motion, "gyroscopeScale")?,
            });
        }
        if let Some(pointer) = value.get("pointer").filter(|value| !value.is_null()) {
            calibration.pointer = Some(PointerSetup {
                sensor_bar: match pointer
                    .get("sensorBar")
                    .and_then(Value::as_str)
                    .unwrap_or("above")
                {
                    "below" => SensorBarPosition::Below,
                    _ => SensorBarPosition::Above,
                },
                sensitivity: match pointer
                    .get("sensitivity")
                    .and_then(Value::as_u64)
                    .unwrap_or(3)
                {
                    1 => IrSensitivity::Level1,
                    2 => IrSensitivity::Level2,
                    4 => IrSensitivity::Level4,
                    5 => IrSensitivity::Level5,
                    _ => IrSensitivity::Level3,
                },
                mode: match pointer
                    .get("mode")
                    .and_then(Value::as_str)
                    .unwrap_or("basic")
                {
                    "extended" => IrMode::Extended,
                    "full" => IrMode::Full,
                    _ => IrMode::Basic,
                },
            });
        }
        Ok(calibration)
    }

    fn stick(value: &Value) -> Result<StickCalibration, String> {
        Ok(StickCalibration {
            center: Self::array(value, "center")?,
            min: Self::array(value, "min")?,
            max: Self::array(value, "max")?,
            dead_zone: Self::f32(value, "deadZone")?,
            outer_dead_zone: Self::f32(value, "outerDeadZone")?,
        })
    }

    fn trigger(value: &Value) -> Result<TriggerCalibration, String> {
        Ok(TriggerCalibration {
            min: Self::number(value, "min")?,
            max: Self::number(value, "max")?,
            dead_zone: Self::f32(value, "deadZone")?,
        })
    }

    fn array<T: serde::de::DeserializeOwned>(value: &Value, name: &str) -> Result<T, String> {
        serde_json::from_value(
            value
                .get(name)
                .cloned()
                .ok_or_else(|| format!("{name} is required"))?,
        )
        .map_err(|error| error.to_string())
    }

    fn number<T: TryFrom<u64>>(value: &Value, name: &str) -> Result<T, String> {
        value
            .get(name)
            .and_then(Value::as_u64)
            .and_then(|value| T::try_from(value).ok())
            .ok_or_else(|| format!("{name} is invalid"))
    }

    fn f32(value: &Value, name: &str) -> Result<f32, String> {
        value
            .get(name)
            .and_then(Value::as_f64)
            .map(|value| value as f32)
            .filter(|value| value.is_finite())
            .ok_or_else(|| format!("{name} is invalid"))
    }

    fn calibration_value(value: &Calibration) -> Value {
        serde_json::json!({ "sticks": value.sticks.map(|value| value.map(|value| serde_json::json!({ "center": value.center, "min": value.min, "max": value.max, "deadZone": value.dead_zone, "outerDeadZone": value.outer_dead_zone }))), "triggers": value.triggers.map(|value| value.map(|value| serde_json::json!({ "min": value.min, "max": value.max, "deadZone": value.dead_zone }))), "motion": value.motion.map(|value| serde_json::json!({ "accelerometerBias": value.accelerometer_bias, "accelerometerScale": value.accelerometer_scale, "gyroscopeBias": value.gyroscope_bias, "gyroscopeScale": value.gyroscope_scale })), "pointer": value.pointer.map(|value| serde_json::json!({ "sensorBar": match value.sensor_bar { SensorBarPosition::Above => "above", SensorBarPosition::Below => "below" }, "sensitivity": match value.sensitivity { IrSensitivity::Level1 => 1, IrSensitivity::Level2 => 2, IrSensitivity::Level3 => 3, IrSensitivity::Level4 => 4, IrSensitivity::Level5 => 5 }, "mode": match value.mode { IrMode::Basic => "basic", IrMode::Extended => "extended", IrMode::Full => "full" } })) })
    }
}

impl FrontendServices {
    pub fn new(
        plugins: Arc<snowglass_pluginhost::PluginRuntime>,
        configuration: Option<ConfigurationDefaults>,
        input_sources: InputHub,
        controller_api: ControllerApiState,
        platform: Arc<dyn RuntimePlatform>,
        notifications: SnowglassNotificationSender,
    ) -> Self {
        let deno_input = Arc::new(DenoInputListener::new(
            input_sources.clone(),
            notifications.clone(),
        ));
        let input_listener = input_sources.add_listener(deno_input.clone());
        Self {
            plugins,
            configuration,
            storage: crate::console_storage_manager(),
            prepared_filesystems: PreparedFilesystemStore::default(),
            controller_sessions: ControllerSessionStore::default(),
            input_sources,
            controller_api,
            platform,
            notifications,
            deno_input,
            input_listener,
            calibration_captures: Mutex::new(HashMap::new()),
        }
    }
}

impl SnowglassHost for FrontendServices {
    fn scrape(&self, path: String) -> ApiFuture<Result<Value, String>> {
        Box::pin(async move {
            tokio::task::spawn_blocking(move || crate::ScrapedPackage::from_path(path))
                .await
                .map_err(|error| format!("package detection task failed: {error}"))?
                .map_err(|error| error.to_string())
                .and_then(|package| {
                    serde_json::to_value(package).map_err(|error| error.to_string())
                })
        })
    }

    fn describe_configuration(
        &self,
        package: Value,
        preferred_emulator: Option<String>,
    ) -> ApiFuture<Result<Value, String>> {
        let defaults = match self.configuration.clone() {
            Some(defaults) => defaults,
            None => {
                return Box::pin(async { Err("configuration service is unavailable".to_owned()) });
            }
        };
        let plugins = self.plugins.handle();
        Box::pin(async move {
            crate::describe_configuration(plugins, defaults, package, preferred_emulator).await
        })
    }

    fn configure(
        &self,
        package: Value,
        emulator: String,
        user_configuration: Option<Value>,
    ) -> ApiFuture<Result<Value, String>> {
        let plugins = self.plugins.handle();
        Box::pin(async move {
            crate::configuration::configure_api(plugins, package, emulator, user_configuration)
                .await
        })
    }

    fn prepare_configuration(&self, package: Value, request: Value) -> Result<Value, String> {
        let defaults = self
            .configuration
            .clone()
            .ok_or_else(|| "configuration service is unavailable".to_owned())?;
        let controller_profiles = self.controller_api.controller_profiles();
        let controller_assignments = self.controller_api.controller_assignments();
        crate::configuration::prepare_api_with_sources(
            self.plugins.handle().registry().clone(),
            defaults,
            package,
            request,
            &self.input_sources,
            &controller_profiles,
            &controller_assignments,
        )
    }

    fn controller_api_snapshot(&self) -> Value {
        self.platform.controller_api_snapshot()
    }

    fn controller_api_command(&self, command: Value) -> ApiFuture<Result<Value, String>> {
        self.platform.controller_api_command(command)
    }

    fn input_get(&self, index: u8) -> Option<Value> {
        self.deno_input.get(index)
    }

    fn controller_device_command(&self, command: Value) -> Result<Value, String> {
        ControllerDeviceCommandDecoder::new(command).execute(
            &self.input_sources,
            &self.input_listener,
            &self.calibration_captures,
        )
    }

    fn list_plugins(&self) -> Value {
        serde_json::to_value(self.plugins.handle().registry().descriptors())
            .expect("plugin descriptors are JSON-compatible")
    }

    fn invoke_plugin(
        &self,
        plugin: String,
        path: Vec<String>,
        arguments: Vec<Value>,
    ) -> ApiFuture<Value> {
        let handle = self.plugins.handle();
        let storage = self.storage.clone();
        let prepared_filesystems = self.prepared_filesystems.clone();
        let controller_sessions = self.controller_sessions.clone();
        let notifications = self.notifications.clone();
        Box::pin(async move {
            let (quick_resume, quick_resume_cache) =
                prepare_quick_resume(&handle, &plugin, &path, &arguments).await;
            let storage =
                match crate::launch_storage(storage, &path, &arguments, notifications).await {
                    Ok(storage) => storage,
                    Err(error) => {
                        let error = snowglass_pluginhost::PluginInvocationError::new(
                            "Error",
                            "STORAGE_UNAVAILABLE",
                            error,
                        );
                        return serde_json::json!({ "ok": false, "error": error });
                    }
                };
            match handle
                .invoke_emulator_with_storage_owned(
                    &plugin,
                    &path,
                    arguments,
                    storage,
                    quick_resume,
                )
                .await
            {
                Ok(mut result) => {
                    if let Some(filesystem) = result.prepared_filesystem.take() {
                        let token = prepared_filesystems.insert(filesystem);
                        if let Some(object) = result.value.as_object_mut() {
                            object.insert("preparedFilesystem".to_owned(), Value::String(token));
                        }
                    }
                    if let Some(session) = result.controller_session.take() {
                        let token = controller_sessions.insert(session);
                        if let Some(object) = result.value.as_object_mut() {
                            object.insert("controllerSession".to_owned(), Value::String(token));
                        }
                    }
                    if let Some(cache) = quick_resume_cache
                        && let Some(object) = result.value.as_object_mut()
                    {
                        object.insert("quickResumeCache".to_owned(), cache);
                    }
                    serde_json::json!({ "ok": true, "value": result.value })
                }
                Err(error) => serde_json::json!({ "ok": false, "error": error }),
            }
        })
    }

    fn release_prepared_filesystem(&self, token: String) -> bool {
        self.prepared_filesystems.release(&token)
    }

    fn consolestorage(&self, request: ConsoleStorageRequest) -> ApiFuture<Result<Value, String>> {
        let storage = self.storage.clone();
        Box::pin(async move { crate::serve_console_storage(storage, request).await })
    }

    fn emulator_launch(
        &self,
        token: InstanceToken,
        options: Value,
    ) -> ApiFuture<Result<(), String>> {
        let prepared = match options.get("preparedFilesystem").and_then(Value::as_str) {
            Some(handle) => match self.prepared_filesystems.take(handle) {
                Some(filesystem) => Some(filesystem),
                None => {
                    return Box::pin(async {
                        Err(
                            "prepared filesystem handle is unknown or was already consumed"
                                .to_owned(),
                        )
                    });
                }
            },
            None => None,
        };
        let controller_session = match options.get("controllerSession").and_then(Value::as_str) {
            Some(handle) => match self.controller_sessions.take(handle) {
                Some(session) => Some(session),
                None => {
                    return Box::pin(async {
                        Err(
                            "controller session handle is unknown or was already consumed"
                                .to_owned(),
                        )
                    });
                }
            },
            None => None,
        };
        self.platform
            .emulator_launch(token, options, prepared, controller_session)
    }

    fn emulator_close(&self, token: InstanceToken) {
        self.platform.emulator_close(token);
    }

    fn emulator_set_volume(&self, token: InstanceToken, volume: f64) {
        self.platform.emulator_set_volume(token, volume);
    }

    fn emulator_set_muted(&self, token: InstanceToken, muted: bool) {
        self.platform.emulator_set_muted(token, muted);
    }

    fn emulator_message(&self, token: InstanceToken, name: String, args: Value) {
        self.platform.emulator_message(token, name, args);
    }

    fn emulator_input(&self, token: InstanceToken, active: bool) {
        self.platform.emulator_input(token, active);
    }

    fn emulator_pause_state(
        &self,
        token: InstanceToken,
        pause: bool,
        timeout: Duration,
    ) -> ApiFuture<Result<(), String>> {
        self.platform.emulator_pause_state(token, pause, timeout)
    }

    fn emulator_save_state(
        &self,
        token: InstanceToken,
        timeout: Duration,
    ) -> ApiFuture<Result<Value, String>> {
        self.platform.emulator_save_state(token, timeout)
    }

    fn emulator_resume_state(
        &self,
        token: InstanceToken,
        slot: u32,
        timeout: Duration,
    ) -> ApiFuture<Result<Value, String>> {
        self.platform.emulator_resume_state(token, slot, timeout)
    }

    fn emulator_state_screenshot(
        &self,
        token: InstanceToken,
        record_id: String,
    ) -> ApiFuture<Result<Option<Vec<u8>>, String>> {
        self.platform.emulator_state_screenshot(token, record_id)
    }

    fn input_configure(&self, request_id: u32, transaction: Value) {
        self.platform.input_configure(request_id, transaction);
    }

    fn webrtc_start(
        &self,
        token: InstanceToken,
        bind: SocketAddr,
        targets: Vec<u32>,
    ) -> ApiFuture<Result<(), String>> {
        self.platform.webrtc_start(token, bind, targets)
    }

    fn webrtc_stop(&self, token: InstanceToken) -> ApiFuture<Result<(), String>> {
        self.platform.webrtc_stop(token)
    }
}

async fn prepare_quick_resume(
    plugins: &snowglass_pluginhost::PluginRuntimeHandle,
    plugin_id: &str,
    path: &[String],
    arguments: &[Value],
) -> (Option<snowglass_pluginhost::QuickResumeData>, Option<Value>) {
    if !matches!(path, [name] if name == "onLaunch")
        && !matches!(path, [root, name] if root == "default" && name == "onLaunch")
    {
        return (None, None);
    }
    let Some(plugin) = plugins
        .registry()
        .plugin::<dyn EmulatorPluginInstance>(plugin_id)
    else {
        return (None, None);
    };
    let Some(revision) = plugin
        .save_states()
        .and_then(|states| states.revision.as_deref())
        .map(str::to_owned)
    else {
        return (None, None);
    };
    let Some(argument) = arguments.first() else {
        return (None, None);
    };
    let package = &argument["package"];
    let platform = package["platform"]
        .as_str()
        .or_else(|| argument["console"].as_str())
        .unwrap_or_default();
    let identity = package["titleId"]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            package["serial"]
                .as_str()
                .filter(|value| !value.trim().is_empty())
        })
        .map(|value| value.trim().to_ascii_lowercase())
        .or_else(|| {
            package["path"].as_str().map(|path| {
                std::fs::canonicalize(path)
                    .unwrap_or_else(|_| std::path::PathBuf::from(path))
                    .to_string_lossy()
                    .to_ascii_lowercase()
            })
        })
        .unwrap_or_default();
    let key = snowglass_library::QuickResumeKey::new(plugin_id, platform, &identity);
    let cache = serde_json::json!({ "key": key.as_str(), "revision": revision });
    let loaded = tokio::task::spawn_blocking({
        let key = key.clone();
        let revision = revision.clone();
        move || snowglass_library::QuickResumeStore::discover()?.take(&key, &revision)
    })
    .await;
    let entry = match loaded {
        Ok(Ok(entry)) => entry,
        Ok(Err(error)) => {
            tracing::warn!(
                plugin = plugin_id,
                "snowglass: loading Quick Resume failed: {error}"
            );
            None
        }
        Err(error) => {
            tracing::warn!(
                plugin = plugin_id,
                "snowglass: Quick Resume task failed: {error}"
            );
            None
        }
    };
    (
        entry.map(|entry| snowglass_pluginhost::QuickResumeData {
            state: entry.state,
            screenshot: entry.screenshot,
        }),
        Some(cache),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unassigned_input_read_rearms_player_notifications() {
        let (notifications, mut inbox) = tokio::sync::mpsc::unbounded_channel();
        let listener = DenoInputListener::new(InputHub::default(), notifications);
        listener.dirty[0].store(true, Ordering::Relaxed);

        assert!(listener.get(0).is_none());

        let device = DeviceId::new(snowglass_input::BackendId::VIRTUAL, "reassigned");
        let state = snowglass_input::DeviceState::default();
        listener.on_input(InputEvent::State {
            index: snowglass_input::PlayerIndex::new(0),
            sequence: 1,
            device: &device,
            state: &state,
        });

        assert!(matches!(
            inbox.try_recv(),
            Ok(snowglass_deno::snowglass::SnowglassNotification::InputChanged { index: 0 })
        ));
    }
}
