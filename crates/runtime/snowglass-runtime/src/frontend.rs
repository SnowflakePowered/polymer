//! Application-facing services used by the Snowglass Deno extension.
//!
//! Domain policy lives here. The shell supplies only thread-affine platform
//! mechanics such as presentation, input routing, and window attachment.

use crate::ConfigurationDefaults;
use parking_lot::{Mutex, RwLock};
use serde_json::Value;
use snowglass_deno::snowglass::{
    ApiFuture, ConsoleStorageRequest, InstanceToken, SnowglassHost, SnowglassNotificationSender,
};
use snowglass_input::source::InputSourceCatalog;
use snowglass_library::StorageManager;
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

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
    ) -> ApiFuture<Result<(), String>>;
    fn emulator_close(&self, token: InstanceToken);
    fn emulator_set_volume(&self, token: InstanceToken, volume: f64);
    fn emulator_set_muted(&self, token: InstanceToken, muted: bool);
    fn emulator_message(&self, token: InstanceToken, name: String, args: Value);
    fn emulator_input(&self, token: InstanceToken, active: bool);
    fn input_configure(&self, request_id: u32, transaction: Value);
    fn input_api_snapshot(&self) -> Value;
    fn input_api_command(&self, command: Value) -> ApiFuture<Result<Value, String>>;
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
}

#[derive(Debug)]
pub enum InputCommand {
    Configure {
        request_id: u32,
        transaction: Value,
    },
    Api {
        command: Value,
        result: tokio::sync::oneshot::Sender<Result<Value, String>>,
    },
}

#[derive(Clone)]
pub struct InputApiState {
    state: Arc<RwLock<Value>>,
    controller_profiles: crate::ControllerProfiles,
}

impl Default for InputApiState {
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
            controller_profiles: crate::ControllerProfiles::default(),
        }
    }
}

impl InputApiState {
    pub fn snapshot(&self) -> Value {
        self.state.read().clone()
    }

    pub fn controller_profiles(&self) -> crate::ControllerProfiles {
        self.controller_profiles.clone()
    }

    pub fn publish(&self, controllers: Value) {
        let mut state = self.state.write();
        if state["controllers"] == controllers {
            return;
        }
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
    input_api: InputApiState,
    webrtc: WebRtcCommandSink,
}

impl CommandRuntimePlatform {
    pub fn new(
        emulator: EmulatorCommandSink,
        input: InputCommandSink,
        input_api: InputApiState,
        webrtc: WebRtcCommandSink,
    ) -> Self {
        Self {
            emulator,
            input,
            input_api,
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
    ) -> ApiFuture<Result<(), String>> {
        let sink = self.emulator.clone();
        Box::pin(async move {
            let (result, response) = tokio::sync::oneshot::channel();
            sink.send(EmulatorCommand::Launch {
                token,
                options,
                prepared_filesystem,
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

    fn input_configure(&self, request_id: u32, transaction: Value) {
        self.input.send(InputCommand::Configure {
            request_id,
            transaction,
        });
    }

    fn input_api_snapshot(&self) -> Value {
        self.input_api.snapshot()
    }

    fn input_api_command(&self, command: Value) -> ApiFuture<Result<Value, String>> {
        let sink = self.input.clone();
        Box::pin(async move {
            let (result, response) = tokio::sync::oneshot::channel();
            sink.send(InputCommand::Api { command, result });
            response
                .await
                .map_err(|_| "input host is unavailable".to_owned())?
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
    input_sources: InputSourceCatalog,
    controller_profiles: crate::ControllerProfiles,
    platform: Arc<dyn RuntimePlatform>,
    notifications: SnowglassNotificationSender,
}

impl FrontendServices {
    pub fn new(
        plugins: Arc<snowglass_pluginhost::PluginRuntime>,
        configuration: Option<ConfigurationDefaults>,
        input_sources: InputSourceCatalog,
        controller_profiles: crate::ControllerProfiles,
        platform: Arc<dyn RuntimePlatform>,
        notifications: SnowglassNotificationSender,
    ) -> Self {
        Self {
            plugins,
            configuration,
            storage: crate::console_storage_manager(),
            prepared_filesystems: PreparedFilesystemStore::default(),
            input_sources,
            controller_profiles,
            platform,
            notifications,
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
    ) -> Result<Value, String> {
        let defaults = self
            .configuration
            .clone()
            .ok_or_else(|| "configuration service is unavailable".to_owned())?;
        crate::describe_configuration(
            self.plugins.handle().registry().clone(),
            defaults,
            package,
            preferred_emulator,
        )
    }

    fn prepare_configuration(&self, package: Value, request: Value) -> Result<Value, String> {
        let defaults = self
            .configuration
            .clone()
            .ok_or_else(|| "configuration service is unavailable".to_owned())?;
        crate::configuration::prepare_api_with_sources(
            self.plugins.handle().registry().clone(),
            defaults,
            package,
            request,
            &self.input_sources,
            &self.controller_profiles,
        )
    }

    fn input_api_snapshot(&self) -> Value {
        self.platform.input_api_snapshot()
    }

    fn input_api_command(&self, command: Value) -> ApiFuture<Result<Value, String>> {
        self.platform.input_api_command(command)
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
        let notifications = self.notifications.clone();
        Box::pin(async move {
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
                .invoke_with_storage_owned(&plugin, &path, arguments, storage)
                .await
            {
                Ok(mut result) => {
                    if let Some(filesystem) = result.prepared_filesystem.take() {
                        let token = prepared_filesystems.insert(filesystem);
                        if let Some(object) = result.value.as_object_mut() {
                            object.insert("preparedFilesystem".to_owned(), Value::String(token));
                        }
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
        self.platform.emulator_launch(token, options, prepared)
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
