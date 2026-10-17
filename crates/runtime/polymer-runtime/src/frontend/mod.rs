//! Application-facing services used by the Polymer Deno extension.
//!
//! Domain policy lives here. The shell supplies only thread-affine platform
//! mechanics such as presentation, input routing, and window attachment.

mod commands;
pub mod emulator;
pub mod input;
mod packages;
mod quickresume;
pub use commands::{EmulatorRecipients, FrontendCommands, FrontendRecipients, InputRecipients};

use crate::ControllerProfiles;
use crate::{ConfigurationDefaults, LibraryService};
use parking_lot::{Mutex, RwLock};
use polymer_config::input::gamepad::StandardGamepad;
use polymer_deno::polymer::{
    ApiFuture, ConsoleStorage, InstanceToken, Library, PolymerHost, PolymerNotification,
    PolymerNotificationSender,
};
use polymer_input::{
    Calibration, CalibrationCapture, CalibrationKind, DeviceId, Feedback, InputEvent, InputHub,
    InputListener, IrMode, IrSensitivity, ListenerHandle, MotionCalibration, PointerSetup,
    SensorBarPosition, StickCalibration, TriggerCalibration,
};
use polymer_library::StorageManager;
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

const CONTROLLER_API_RESPONSE_ATTEMPTS: usize = 3;
const CONTROLLER_API_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
const CONTROLLER_CHANGE_RESPONSE_ATTEMPTS: usize =
    polymer_controllers::DEFAULT_CONTROLLER_TOPOLOGY_TRANSACTION_TIMEOUT.as_secs() as usize
        / CONTROLLER_API_RESPONSE_TIMEOUT.as_secs() as usize
        + 1;

#[derive(Clone)]
pub struct ControllerApiState {
    state: Arc<RwLock<Value>>,
    controller_profiles: ControllerProfiles,
    controller_assignments: Arc<RwLock<crate::ControllerAssignmentSnapshot>>,
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

    pub(crate) fn controller_assignments(&self) -> crate::ControllerAssignmentSnapshot {
        self.controller_assignments.read().clone()
    }

    pub fn revision(&self) -> u64 {
        self.state.read()["revision"].as_u64().unwrap_or_default()
    }

    pub fn controllers(&self) -> Value {
        self.state.read()["controllers"].clone()
    }

    pub fn publish(&self, controllers: &Value, assignments: crate::ControllerAssignmentSnapshot) {
        let mut state = self.state.write();
        let published = &state["controllers"];
        let unchanged = controllers.as_object().is_some_and(|next| {
            next.iter()
                .all(|(key, value)| published.get(key) == Some(value))
        });
        if unchanged {
            return;
        }
        let mut controllers = controllers.clone();
        controllers["layouts"] =
            serde_json::to_value(polymer_controllers::layout::InputLayout::all())
                .unwrap_or_default();
        controllers["pad"] = serde_json::json!({
            "buttons": StandardGamepad::BUTTONS,
            "axes": StandardGamepad::axis_sources().collect::<Vec<_>>(),
        });
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

impl FrontendCommands {
    fn controller_api(&self, command: Value) -> ApiFuture<Result<Value, String>> {
        let client = self.clone();
        Box::pin(async move {
            let response_attempts =
                if command["type"] == polymer_controllers::CONTROLLER_SESSION_COMMIT_COMMAND {
                    // Live emulator reconfiguration can include several UIA
                    // operations. Give it enough time to reach the plugin's
                    // 60-second deadline plus one response interval.
                    CONTROLLER_CHANGE_RESPONSE_ATTEMPTS
                } else {
                    CONTROLLER_API_RESPONSE_ATTEMPTS
                };
            let mut response = client.request(
                |recipients| &recipients.input.controller_api,
                input::ControllerApi { command },
                "controller host is unavailable",
            );
            for attempt in 1..=response_attempts {
                match tokio::time::timeout(CONTROLLER_API_RESPONSE_TIMEOUT, &mut response).await {
                    Ok(result) => {
                        return result;
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
}
#[derive(Clone, Default)]
struct PreparedFilesystemStore(Arc<PreparedFilesystemStoreInner>);

#[derive(Default)]
struct PreparedFilesystemStoreInner {
    next: AtomicU64,
    filesystems: Mutex<HashMap<u64, polymer_vfs::PreparedFilesystemLaunch>>,
}

#[derive(Clone, Default)]
struct ControllerSessionStore(Arc<ControllerSessionStoreInner>);

#[derive(Default)]
struct ControllerSessionStoreInner {
    next: AtomicU64,
    sessions: Mutex<HashMap<u64, polymer_pluginhost::controller::ControllerSession>>,
}

impl ControllerSessionStore {
    fn insert(&self, session: polymer_pluginhost::controller::ControllerSession) -> String {
        let token = self.0.next.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        self.0.sessions.lock().insert(token, session);
        format!("{token:016x}")
    }

    fn take(&self, token: &str) -> Option<polymer_pluginhost::controller::ControllerSession> {
        let token = u64::from_str_radix(token, 16).ok()?;
        self.0.sessions.lock().remove(&token)
    }
}

impl PreparedFilesystemStore {
    fn insert(&self, filesystem: polymer_vfs::PreparedFilesystemLaunch) -> String {
        let token = self.0.next.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        self.0.filesystems.lock().insert(token, filesystem);
        format!("{token:016x}")
    }

    fn take(&self, token: &str) -> Option<polymer_vfs::PreparedFilesystemLaunch> {
        let token = u64::from_str_radix(token, 16).ok()?;
        self.0.filesystems.lock().remove(&token)
    }

    fn release(&self, token: &str) -> bool {
        self.take(token).is_some()
    }
}

/// Runtime-owned implementation of all non-DOM `Polymer.*` services.
pub struct FrontendServices {
    plugins: Arc<polymer_pluginhost::runtime::PluginRuntime>,
    configuration: Option<ConfigurationDefaults>,
    library: Option<LibraryService>,
    launchable_configurations:
        Option<Arc<crate::launchable_configuration::LaunchableConfigurations>>,
    storage: Option<Arc<StorageManager>>,
    storage_service: Result<crate::StorageService, String>,
    job_service: Result<crate::JobService, String>,
    prepared_filesystems: PreparedFilesystemStore,
    controller_sessions: ControllerSessionStore,
    input_sources: InputHub,
    controller_api: ControllerApiState,
    commands: FrontendCommands,
    notifications: PolymerNotificationSender,
    deno_input: Arc<DenoInputListener>,
    input_listener: ListenerHandle,
    calibration_captures: Mutex<HashMap<DeviceId, CalibrationCapture>>,
    shaders: polymer_library::ShaderLibrary,
}

struct DenoInputListener {
    hub: InputHub,
    notifications: PolymerNotificationSender,
    dirty: [AtomicBool; 8],
}

impl DenoInputListener {
    fn new(hub: InputHub, notifications: PolymerNotificationSender) -> Self {
        Self {
            hub,
            notifications,
            dirty: std::array::from_fn(|_| AtomicBool::new(false)),
        }
    }

    fn get(&self, index: u8) -> Option<Value> {
        let index = polymer_input::PlayerIndex::new(index);
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
        use polymer_deno::polymer::PolymerNotification;
        let InputEvent::State { index, .. } = event else {
            return;
        };
        if self.dirty[usize::from(index.get())].swap(true, Ordering::AcqRel) {
            return;
        }
        let _ = self
            .notifications
            .send(PolymerNotification::InputChanged { index: index.get() });
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

    fn index(&self) -> Result<polymer_input::PlayerIndex, String> {
        Ok(polymer_input::PlayerIndex::new(self.u8("index")?))
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
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        plugins: Arc<polymer_pluginhost::runtime::PluginRuntime>,
        configuration: Option<ConfigurationDefaults>,
        library: Option<LibraryService>,
        local_storage: Option<polymer_deno::webstorage::LocalStorage>,
        input_sources: InputHub,
        controller_api: ControllerApiState,
        commands: FrontendCommands,
        notifications: PolymerNotificationSender,
        shaders: polymer_library::ShaderLibrary,
    ) -> Self {
        let deno_input = Arc::new(DenoInputListener::new(
            input_sources.clone(),
            notifications.clone(),
        ));
        let input_listener = input_sources.add_listener(deno_input.clone());
        if let Some(library) = &library {
            let mut sources = library.subscribe_sources();
            let source_notifications = notifications.clone();
            tokio::spawn(async move {
                loop {
                    let statuses = match sources.recv().await {
                        Ok(statuses) => statuses,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    };
                    if source_notifications
                        .send(PolymerNotification::LibrarySourcesChanged(statuses))
                        .is_err()
                    {
                        break;
                    }
                }
            });
            let mut events = library.subscribe();
            let notifications = notifications.clone();
            tokio::spawn(async move {
                loop {
                    let event = match events.recv().await {
                        Ok(event) => event,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    };
                    let notification = match event {
                        polymer_scraping::ScrapingEvent::LibraryContentChanged {
                            root_binary_id,
                        } => PolymerNotification::LibraryContentChanged {
                            root_binary_id: root_binary_id.simple().to_string(),
                        },
                    };
                    if notifications.send(notification).is_err() {
                        break;
                    }
                }
            });
        }
        let mut plugin_events = polymer_pluginhost::runtime::PluginEvent::subscribe();
        let plugin_notifications = notifications.clone();
        tokio::spawn(async move {
            loop {
                let event = tokio::select! {
                    _ = plugin_notifications.closed() => break,
                    event = plugin_events.recv() => match event {
                        Some(event) => event,
                        None => break,
                    },
                };
                let notification = PolymerNotification::PluginEvent(
                    serde_json::to_value(event).expect("plugin events are JSON-compatible"),
                );
                if plugin_notifications.send(notification).is_err() {
                    break;
                }
            }
        });
        plugins
            .handle()
            .load::<dyn polymer_pluginhost::emulator::EmulatorPluginInstance>();
        plugins
            .handle()
            .load::<dyn polymer_pluginhost::platform::PlatformPluginInstance>();
        let launchable_configurations =
            library
                .as_ref()
                .zip(local_storage)
                .map(|(library, storage)| {
                    Arc::new(
                        crate::launchable_configuration::LaunchableConfigurations::new(
                            library.catalog(),
                            storage,
                            plugins.handle(),
                            library.configuration_locks(),
                        ),
                    )
                });
        let storage = polymer_library::StorageManager::shared().ok();
        let storage_service = crate::StorageService::start(
            storage.clone(),
            plugins.handle(),
            library.as_ref().map(LibraryService::catalog),
        )
        .map_err(|error| format!("could not start storage actor: {error}"));
        let jobs = plugins.handle().jobs().cloned();
        if let Some(jobs) = &jobs {
            let mut changes = jobs.subscribe();
            let notifications = notifications.clone();
            tokio::spawn(async move {
                loop {
                    let job = match changes.recv().await {
                        Ok(job) => job,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    };
                    let notification = PolymerNotification::JobChanged(
                        serde_json::to_value(job).expect("jobs are serializable"),
                    );
                    if notifications.send(notification).is_err() {
                        break;
                    }
                }
            });
        }
        let job_service = crate::JobService::start(
            jobs,
            storage_service.as_ref().ok().cloned(),
            library.as_ref().map(LibraryService::client),
            library.as_ref().map(LibraryService::catalog),
        )
        .map_err(|error| format!("could not start jobs actor: {error}"));
        Self {
            plugins,
            configuration,
            library,
            launchable_configurations,
            storage,
            storage_service,
            job_service,
            prepared_filesystems: PreparedFilesystemStore::default(),
            controller_sessions: ControllerSessionStore::default(),
            input_sources,
            controller_api,
            commands,
            notifications,
            deno_input,
            input_listener,
            calibration_captures: Mutex::new(HashMap::new()),
            shaders,
        }
    }
}

impl PolymerHost for FrontendServices {
    fn scrape(&self, path: String) -> ApiFuture<Result<Value, String>> {
        Box::pin(async move {
            tokio::task::spawn_blocking(move || polymer_scraping::Package::from_path(path))
                .await
                .map_err(|error| format!("package detection task failed: {error}"))?
                .map_err(|error| error.to_string())
                .and_then(|package| {
                    serde_json::to_value(package).map_err(|error| error.to_string())
                })
        })
    }

    fn digest(
        &self,
        uri: String,
        binary: bool,
        algorithm: String,
    ) -> ApiFuture<Result<String, String>> {
        use polymer_pluginhost::digest::{DigestAlgorithm, Digestible, PackageBinary};
        Box::pin(async move {
            let algorithm: DigestAlgorithm = serde_json::from_value(Value::String(algorithm))
                .map_err(|error| error.to_string())?;
            let uri: polymer_library::LaunchableUri = uri
                .parse()
                .map_err(|error: polymer_library::GameCatalogError| error.to_string())?;
            tokio::task::spawn_blocking(move || {
                if binary {
                    PackageBinary(&uri.package_path()?).digest(algorithm)
                } else {
                    uri.binary_path()?.as_path().digest(algorithm)
                }
            })
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())
        })
    }

    fn describe_configuration(
        &self,
        launchable_id: String,
        preferred_emulator: Option<String>,
    ) -> ApiFuture<Result<Value, String>> {
        let service = self.launchable_configurations.clone();
        let defaults = self.configuration.clone();
        Box::pin(async move {
            service
                .ok_or("library configuration service is unavailable")?
                .describe(
                    &launchable_id,
                    defaults.ok_or("configuration service is unavailable")?,
                    preferred_emulator,
                )
                .await
        })
    }

    fn configure(
        &self,
        launchable_id: String,
        emulator: String,
        user_configuration: Option<Value>,
    ) -> ApiFuture<Result<Value, String>> {
        let service = self.launchable_configurations.clone();
        Box::pin(async move {
            service
                .ok_or("library configuration service is unavailable")?
                .configure(&launchable_id, &emulator, user_configuration)
                .await
        })
    }

    fn select_emulator(
        &self,
        launchable_id: String,
        emulator: Option<String>,
    ) -> ApiFuture<Result<(), String>> {
        let service = self.launchable_configurations.clone();
        Box::pin(async move {
            service
                .ok_or("library configuration service is unavailable")?
                .select(&launchable_id, emulator)
                .await
        })
    }

    fn platform_settings(&self) -> ApiFuture<Result<Value, String>> {
        let service = self.launchable_configurations.clone();
        Box::pin(async move {
            service
                .ok_or("library configuration service is unavailable")?
                .platforms()
                .await
        })
    }

    fn update_platform_settings(
        &self,
        platform: String,
        settings: polymer_deno::polymer::PlatformSettings,
    ) -> ApiFuture<Result<Value, String>> {
        let service = self.launchable_configurations.clone();
        Box::pin(async move {
            service
                .ok_or("library configuration service is unavailable")?
                .update_platform(&platform, settings)
                .await
        })
    }

    fn prepare_configuration(
        &self,
        launchable_id: String,
        mut request: Value,
    ) -> ApiFuture<Result<Value, String>> {
        let service = self.launchable_configurations.clone();
        let defaults = self.configuration.clone();
        let plugins = self.plugins.handle().registry().clone();
        let sources = self.input_sources.clone();
        let profiles = self.controller_api.controller_profiles();
        let assignments = self.controller_api.controller_assignments();
        Box::pin(async move {
            let service = service.ok_or("library configuration service is unavailable")?;
            let emulator = request["emulator"]
                .as_str()
                .ok_or("missing emulator")?
                .to_owned();
            let configuration = service.configure(&launchable_id, &emulator, None).await?;
            request["userConfiguration"] = configuration
                .get("values")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            let ids = match request.get("launchSet") {
                Some(value) => serde_json::from_value::<Vec<String>>(value.clone())
                    .map_err(|error| error.to_string())?,
                None => vec![launchable_id.clone()],
            };
            if ids.first() != Some(&launchable_id) {
                return Err("configuration launchable must be the first launchSet entry".into());
            }
            let discs = service.launch_set(&ids).await?;
            let initial_index = request
                .get("initialDiscIndex")
                .map_or(Some(0), Value::as_u64)
                .filter(|index| *index < discs.len() as u64)
                .ok_or("initialDiscIndex is outside the launch set")?;
            let package = discs[0].1.clone();
            let mut prepared = crate::configuration::prepare_api_with_sources(
                plugins,
                defaults.ok_or("configuration service is unavailable")?,
                package,
                request,
                &sources,
                &profiles,
                &assignments,
                &launchable_id,
            )?;
            prepared["launchSet"] =
                Value::Array(discs.iter().map(|(record, _)| record.clone()).collect());
            prepared["arguments"]["launchSet"] = Value::Array(
                discs
                    .iter()
                    .map(|(_, package)| package.launch_entry())
                    .collect(),
            );
            prepared["arguments"]["initialDiscIndex"] = initial_index.into();
            Ok(prepared)
        })
    }

    fn controller_api_snapshot(&self) -> Value {
        self.controller_api.snapshot()
    }

    fn controller_api_command(&self, command: Value) -> ApiFuture<Result<Value, String>> {
        self.commands.controller_api(command)
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

    fn plugin_options(&self, plugin: String) -> ApiFuture<Result<Value, String>> {
        let handle = self.plugins.handle();
        Box::pin(async move {
            handle
                .options(&plugin)
                .await
                .map(Value::Object)
                .map_err(|error| error.message)
        })
    }

    fn set_plugin_option(
        &self,
        plugin: String,
        key: String,
        value: Value,
    ) -> ApiFuture<Result<(), String>> {
        let handle = self.plugins.handle();
        Box::pin(async move {
            handle
                .set_option(&plugin, &key, value)
                .await
                .map_err(|error| error.message)
        })
    }

    fn plugin_cache_size(&self, plugin: String, key: String) -> ApiFuture<Result<u64, String>> {
        let handle = self.plugins.handle();
        Box::pin(async move {
            handle
                .cache_size(&plugin, &key)
                .await
                .map_err(|error| error.message)
        })
    }

    fn clear_plugin_cache(&self, plugin: String, key: String) -> ApiFuture<Result<(), String>> {
        let handle = self.plugins.handle();
        Box::pin(async move {
            handle
                .clear_cache(&plugin, &key)
                .await
                .map_err(|error| error.message)
        })
    }

    fn invoke_plugin(
        &self,
        plugin: String,
        path: Vec<String>,
        mut arguments: Vec<Value>,
    ) -> ApiFuture<Value> {
        let handle = self.plugins.handle();
        let storage = self.storage.clone();
        let catalog = self.library.as_ref().map(LibraryService::catalog);
        let prepared_filesystems = self.prepared_filesystems.clone();
        let controller_sessions = self.controller_sessions.clone();
        let notifications = self.notifications.clone();
        Box::pin(async move {
            if matches!(path.as_slice(), [name] if name == "onLaunch")
                || matches!(path.as_slice(), [default, name] if default == "default" && name == "onLaunch")
            {
                arguments = match Self::prepare_launch_packages(arguments).await {
                    Ok(arguments) => arguments,
                    Err(error) => {
                        let error = polymer_pluginhost::runtime::PluginInvocationError::new(
                            "Error",
                            "PACKAGE_PREPARATION_FAILED",
                            error,
                        );
                        return serde_json::json!({ "ok": false, "error": error });
                    }
                };
            }
            let (quick_resume, quick_resume_cache) =
                quickresume::QuickResume::prepare(&handle, &plugin, &path, &mut arguments).await;
            let checkpoints = crate::CheckpointSession::default();
            let storage = match crate::launch_storage(
                storage,
                catalog,
                &path,
                &arguments,
                notifications,
                &checkpoints,
            )
            .await
            {
                Ok(storage) => storage,
                Err(error) => {
                    let error = polymer_pluginhost::runtime::PluginInvocationError::new(
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
                        checkpoints.attach(session.client());
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

    fn consolestorage(&self) -> Result<ConsoleStorage, String> {
        self.storage_service
            .as_ref()
            .map(crate::StorageService::client)
            .map_err(Clone::clone)
    }

    fn shaders(&self) -> Result<polymer_library::ShaderLibrary, String> {
        Ok(self.shaders.clone())
    }

    fn jobs(&self) -> Result<polymer_deno::polymer::Jobs, String> {
        self.job_service
            .as_ref()
            .map(crate::JobService::client)
            .map_err(Clone::clone)
    }

    fn library(&self) -> Result<Library, String> {
        self.library
            .as_ref()
            .map(LibraryService::client)
            .ok_or_else(|| "library service is unavailable".to_owned())
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
        self.commands.request(
            |recipients| &recipients.emulator.launch,
            emulator::Launch {
                token,
                options,
                prepared_filesystem: prepared,
                controller_session,
            },
            "emulator host is unavailable",
        )
    }

    fn emulator_close(&self, token: InstanceToken) {
        self.commands.notify(
            |recipients| &recipients.emulator.close,
            emulator::Close { token },
        );
    }

    fn emulator_set_volume(&self, token: InstanceToken, volume: f64) {
        self.commands.notify(
            |recipients| &recipients.emulator.set_volume,
            emulator::SetVolume { token, volume },
        );
    }

    fn emulator_set_muted(&self, token: InstanceToken, muted: bool) {
        self.commands.notify(
            |recipients| &recipients.emulator.set_muted,
            emulator::SetMuted { token, muted },
        );
    }

    fn emulator_message(&self, token: InstanceToken, name: String, args: Value) {
        self.commands.notify(
            |recipients| &recipients.emulator.message,
            emulator::Message { token, name, args },
        );
    }

    fn emulator_input(&self, token: InstanceToken, active: bool) {
        self.commands.notify(
            |recipients| &recipients.emulator.input,
            emulator::Input { token, active },
        );
    }

    fn emulator_change_disc(
        &self,
        token: InstanceToken,
        index: u32,
    ) -> ApiFuture<Result<(), String>> {
        self.commands.request(
            |recipients| &recipients.emulator.change_disc,
            emulator::ChangeDisc { token, index },
            "emulator host is unavailable",
        )
    }

    fn emulator_pause_state(
        &self,
        token: InstanceToken,
        pause: bool,
        timeout: Duration,
    ) -> ApiFuture<Result<(), String>> {
        self.commands.request(
            |recipients| &recipients.emulator.pause_state,
            emulator::PauseState {
                token,
                pause,
                timeout,
            },
            "emulator host is unavailable",
        )
    }

    fn emulator_state_action(
        &self,
        token: InstanceToken,
        slot: u32,
        save: bool,
        timeout: Duration,
    ) -> ApiFuture<Result<(), String>> {
        self.commands.request(
            |recipients| &recipients.emulator.state_action,
            emulator::StateAction {
                token,
                slot,
                save,
                timeout,
            },
            "emulator host is unavailable",
        )
    }

    fn input_configure(&self, request_id: u32, transaction: Value) {
        self.commands.notify(
            |recipients| &recipients.input.configure,
            input::Configure {
                request_id,
                transaction,
            },
        );
    }
}
