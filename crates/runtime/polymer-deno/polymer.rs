//! Optional user-facing Polymer APIs for a host-owned Deno isolate.
//!
//! The base [`crate::Runtime`] deliberately does not install this extension.
//! Plugin workers therefore retain the curated web platform without gaining
//! access to the application-facing `Polymer` global.

use deno_core::{OpState, op2};
use deno_error::JsErrorBox;
use serde_json::Value;
use std::{
    cell::RefCell,
    collections::HashSet,
    future::{Future, poll_fn},
    net::SocketAddr,
    pin::Pin,
    rc::Rc,
    sync::Arc,
    time::Duration,
};

#[path = "library.rs"]
mod library;

pub use library::{Library, LibraryCommandHandler};
use library::{
    op_library_games_delete, op_library_games_get, op_library_games_list,
    op_library_launchables_add, op_library_launchables_delete, op_library_metadata_add,
    op_library_metadata_delete, op_library_metadata_replace, op_library_metadata_text,
    op_library_metadata_update, op_library_metadata_url, op_scraping_job, op_scraping_jobs,
    op_scraping_refresh, op_scraping_retry, op_scraping_scrapers, op_scraping_start,
};

pub type ApiFuture<T> = Pin<Box<dyn Future<Output = T> + 'static>>;

/// Native capabilities exposed to the application-facing Polymer realm.
///
/// This is the only native interface understood by the frontend extension.
/// Implementations own lifecycle, storage, configuration, and plugin policy;
/// ops only validate their JavaScript inputs and delegate here.
pub trait PolymerHost: Send + Sync {
    fn scrape(&self, path: String) -> ApiFuture<Result<Value, String>>;
    fn describe_configuration(
        &self,
        launchable_id: String,
        preferred_emulator: Option<String>,
    ) -> ApiFuture<Result<Value, String>>;
    fn configure(
        &self,
        launchable_id: String,
        emulator: String,
        user_configuration: Option<Value>,
    ) -> ApiFuture<Result<Value, String>>;
    fn prepare_configuration(
        &self,
        launchable_id: String,
        request: Value,
    ) -> ApiFuture<Result<Value, String>>;
    fn list_plugins(&self) -> Value;
    fn invoke_plugin(
        &self,
        plugin: String,
        path: Vec<String>,
        arguments: Vec<Value>,
    ) -> ApiFuture<Value>;
    fn release_prepared_filesystem(&self, token: String) -> bool;
    fn consolestorage(&self, request: ConsoleStorageRequest) -> ApiFuture<Result<Value, String>>;
    fn library(&self) -> Option<Library>;
    fn emulator_launch(
        &self,
        token: InstanceToken,
        options: Value,
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
    fn input_get(&self, _index: u8) -> Option<Value> {
        None
    }
    fn controller_device_command(&self, _command: Value) -> Result<Value, String> {
        Err("controller devices are unavailable".to_owned())
    }
    fn webrtc_start(
        &self,
        token: InstanceToken,
        bind: SocketAddr,
        targets: Vec<u32>,
    ) -> ApiFuture<Result<(), String>>;
    fn webrtc_stop(&self, token: InstanceToken) -> ApiFuture<Result<(), String>>;
}

#[derive(Debug)]
pub enum ConsoleStorageRequest {
    List,
    Supports {
        kind: String,
        platform: String,
    },
    Create {
        kind: String,
        platform: String,
        label: Option<String>,
    },
    Delete {
        kind: String,
        platform: String,
        id: String,
    },
    SetLabel {
        kind: String,
        platform: String,
        id: String,
        label: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(transparent)]
pub struct InstanceToken(pub u32);

#[derive(Clone, Debug, serde::Serialize)]
#[serde(
    tag = "type",
    content = "detail",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PolymerNotification {
    OpenRequested(Value),
    DropActiveChanged(bool),
    InputConfigurationChanged {
        configuration: Value,
        layouts: Value,
    },
    InputConfigurationSettled {
        request_id: u64,
        result: Value,
    },
    ControllerApiStateChanged(Value),
    InputChanged {
        index: u8,
    },
    EmulatorInstance(Value),
    GameSaved {
        kind: String,
        platform: String,
        id: String,
    },
    PluginEvent(Value),
    ScrapeJobChanged(Value),
    LibraryContentChanged {
        root_binary_id: String,
    },
}

/// Cloneable native-to-JavaScript notification endpoint.
///
/// The receiver lives entirely inside the Deno extension. Native feature
/// producers retain this sender instead of reaching through the shell's DOM
/// and runtime ownership layers.
pub type PolymerNotificationSender = tokio::sync::mpsc::UnboundedSender<PolymerNotification>;

type PolymerNotificationInbox = RefCell<tokio::sync::mpsc::UnboundedReceiver<PolymerNotification>>;

fn notification_channel() -> (PolymerNotificationSender, PolymerNotificationInbox) {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    (sender, RefCell::new(receiver))
}

#[op2]
#[serde]
async fn op_polymer_next_notification(state: Rc<RefCell<OpState>>) -> Option<PolymerNotification> {
    let inbox = state
        .borrow()
        .borrow::<Rc<PolymerNotificationInbox>>()
        .clone();
    poll_fn(|context| inbox.borrow_mut().poll_recv(context)).await
}

#[op2]
#[serde]
async fn op_scraping_package_from_path(
    state: Rc<RefCell<OpState>>,
    #[string] path: String,
) -> Result<serde_json::Value, JsErrorBox> {
    let host = state.borrow().borrow::<Arc<dyn PolymerHost>>().clone();
    host.scrape(path).await.map_err(JsErrorBox::generic)
}

async fn consolestorage(
    state: &Rc<RefCell<OpState>>,
    request: ConsoleStorageRequest,
) -> Result<serde_json::Value, JsErrorBox> {
    let host = state.borrow().borrow::<Arc<dyn PolymerHost>>().clone();
    host.consolestorage(request)
        .await
        .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
async fn op_consolestorage_list(
    state: Rc<RefCell<OpState>>,
) -> Result<serde_json::Value, JsErrorBox> {
    consolestorage(&state, ConsoleStorageRequest::List).await
}

#[op2]
#[serde]
async fn op_consolestorage_supports(
    state: Rc<RefCell<OpState>>,
    #[string] kind: String,
    #[string] platform: String,
) -> Result<serde_json::Value, JsErrorBox> {
    consolestorage(&state, ConsoleStorageRequest::Supports { kind, platform }).await
}

#[op2]
#[serde]
async fn op_consolestorage_create(
    state: Rc<RefCell<OpState>>,
    #[string] kind: String,
    #[string] platform: String,
    #[string] label: Option<String>,
) -> Result<serde_json::Value, JsErrorBox> {
    consolestorage(
        &state,
        ConsoleStorageRequest::Create {
            kind,
            platform,
            label,
        },
    )
    .await
}

#[op2]
#[serde]
async fn op_consolestorage_delete(
    state: Rc<RefCell<OpState>>,
    #[string] kind: String,
    #[string] platform: String,
    #[string] id: String,
) -> Result<serde_json::Value, JsErrorBox> {
    consolestorage(&state, ConsoleStorageRequest::Delete { kind, platform, id }).await
}

#[op2]
#[serde]
async fn op_consolestorage_set_label(
    state: Rc<RefCell<OpState>>,
    #[string] kind: String,
    #[string] platform: String,
    #[string] id: String,
    #[string] label: Option<String>,
) -> Result<serde_json::Value, JsErrorBox> {
    consolestorage(
        &state,
        ConsoleStorageRequest::SetLabel {
            kind,
            platform,
            id,
            label,
        },
    )
    .await
}

#[op2]
#[serde]
async fn op_configuration_describe(
    state: Rc<RefCell<OpState>>,
    #[string] launchable_id: String,
    #[string] preferred_emulator: Option<String>,
) -> Result<serde_json::Value, JsErrorBox> {
    let host = state.borrow().borrow::<Arc<dyn PolymerHost>>().clone();
    host.describe_configuration(launchable_id, preferred_emulator)
        .await
        .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
async fn op_configuration_configure(
    state: Rc<RefCell<OpState>>,
    #[string] launchable_id: String,
    #[string] emulator: String,
    #[serde] user_configuration: Option<serde_json::Value>,
) -> Result<serde_json::Value, JsErrorBox> {
    let host = state.borrow().borrow::<Arc<dyn PolymerHost>>().clone();
    host.configure(launchable_id, emulator, user_configuration)
        .await
        .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
async fn op_configuration_prepare(
    state: Rc<RefCell<OpState>>,
    #[string] launchable_id: String,
    #[serde] request: serde_json::Value,
) -> Result<serde_json::Value, JsErrorBox> {
    let host = state.borrow().borrow::<Arc<dyn PolymerHost>>().clone();
    host.prepare_configuration(launchable_id, request)
        .await
        .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
fn op_plugin_list(state: &mut OpState) -> Result<serde_json::Value, JsErrorBox> {
    Ok(state.borrow::<Arc<dyn PolymerHost>>().list_plugins())
}

#[op2]
#[serde]
async fn op_plugin_invoke(
    state: Rc<RefCell<OpState>>,
    #[string] plugin: String,
    #[serde] path: Vec<String>,
    #[serde] arguments: Vec<serde_json::Value>,
) -> serde_json::Value {
    let host = state.borrow().borrow::<Arc<dyn PolymerHost>>().clone();
    host.invoke_plugin(plugin, path, arguments).await
}

#[op2(fast)]
fn op_prepared_filesystem_release(state: &mut OpState, #[string] token: String) -> bool {
    state
        .borrow::<Arc<dyn PolymerHost>>()
        .release_prepared_filesystem(token)
}

#[op2]
async fn op_emulator_launch(
    state: Rc<RefCell<OpState>>,
    token: u32,
    #[serde] options: serde_json::Value,
) -> Result<(), JsErrorBox> {
    let host = state.borrow().borrow::<Arc<dyn PolymerHost>>().clone();
    host.emulator_launch(InstanceToken(token), options)
        .await
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
fn op_emulator_close(state: &mut OpState, token: u32) {
    state
        .borrow::<Arc<dyn PolymerHost>>()
        .emulator_close(InstanceToken(token));
}

#[op2(fast)]
fn op_emulator_set_volume(state: &mut OpState, token: u32, volume: f64) {
    state
        .borrow::<Arc<dyn PolymerHost>>()
        .emulator_set_volume(InstanceToken(token), volume);
}

#[op2(fast)]
fn op_emulator_set_muted(state: &mut OpState, token: u32, muted: bool) {
    state
        .borrow::<Arc<dyn PolymerHost>>()
        .emulator_set_muted(InstanceToken(token), muted);
}

#[op2]
fn op_emulator_message(
    state: &mut OpState,
    token: u32,
    #[string] name: String,
    #[serde] args: serde_json::Value,
) {
    state
        .borrow::<Arc<dyn PolymerHost>>()
        .emulator_message(InstanceToken(token), name, args);
}

#[op2(fast)]
fn op_emulator_input(state: &mut OpState, token: u32, active: bool) {
    state
        .borrow::<Arc<dyn PolymerHost>>()
        .emulator_input(InstanceToken(token), active);
}

#[op2]
async fn op_emulator_pause_state(
    state: Rc<RefCell<OpState>>,
    token: u32,
    pause: bool,
    timeout_ms: u32,
) -> Result<(), JsErrorBox> {
    let host = state.borrow().borrow::<Arc<dyn PolymerHost>>().clone();
    host.emulator_pause_state(
        InstanceToken(token),
        pause,
        Duration::from_millis(u64::from(timeout_ms)),
    )
    .await
    .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
async fn op_emulator_save_state(
    state: Rc<RefCell<OpState>>,
    token: u32,
    timeout_ms: u32,
) -> Result<serde_json::Value, JsErrorBox> {
    let host = state.borrow().borrow::<Arc<dyn PolymerHost>>().clone();
    host.emulator_save_state(
        InstanceToken(token),
        Duration::from_millis(u64::from(timeout_ms)),
    )
    .await
    .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
async fn op_emulator_resume_state(
    state: Rc<RefCell<OpState>>,
    token: u32,
    slot: u32,
    timeout_ms: u32,
) -> Result<serde_json::Value, JsErrorBox> {
    let host = state.borrow().borrow::<Arc<dyn PolymerHost>>().clone();
    host.emulator_resume_state(
        InstanceToken(token),
        slot,
        Duration::from_millis(u64::from(timeout_ms)),
    )
    .await
    .map_err(JsErrorBox::generic)
}

#[op2]
#[buffer]
async fn op_emulator_state_screenshot(
    state: Rc<RefCell<OpState>>,
    token: u32,
    #[string] record_id: String,
) -> Result<Option<Vec<u8>>, JsErrorBox> {
    let host = state.borrow().borrow::<Arc<dyn PolymerHost>>().clone();
    host.emulator_state_screenshot(InstanceToken(token), record_id)
        .await
        .map_err(JsErrorBox::generic)
}

#[op2]
fn op_input_configure(
    state: &mut OpState,
    request_id: u32,
    #[serde] transaction: serde_json::Value,
) {
    state
        .borrow::<Arc<dyn PolymerHost>>()
        .input_configure(request_id, transaction);
}

#[op2]
#[serde]
fn op_controller_api_snapshot(state: &mut OpState) -> Result<serde_json::Value, JsErrorBox> {
    Ok(state
        .borrow::<Arc<dyn PolymerHost>>()
        .controller_api_snapshot())
}

#[op2]
#[serde]
async fn op_controller_api_command(
    state: Rc<RefCell<OpState>>,
    #[serde] command: serde_json::Value,
) -> Result<serde_json::Value, JsErrorBox> {
    let host = state.borrow().borrow::<Arc<dyn PolymerHost>>().clone();
    host.controller_api_command(command)
        .await
        .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
fn op_input_get(state: &mut OpState, index: u8) -> Option<serde_json::Value> {
    state.borrow::<Arc<dyn PolymerHost>>().input_get(index)
}

#[op2]
#[serde]
fn op_controller_device_command(
    state: &mut OpState,
    #[serde] command: serde_json::Value,
) -> Result<serde_json::Value, JsErrorBox> {
    state
        .borrow::<Arc<dyn PolymerHost>>()
        .controller_device_command(command)
        .map_err(JsErrorBox::type_error)
}

#[op2]
async fn op_webrtc_start(
    state: Rc<RefCell<OpState>>,
    token: u32,
    #[string] bind: String,
    #[serde] targets: Vec<u32>,
) -> Result<(), JsErrorBox> {
    let bind = bind
        .parse::<SocketAddr>()
        .map_err(|error| JsErrorBox::type_error(format!("invalid WebRTC bind address: {error}")))?;
    if targets.is_empty() {
        return Err(JsErrorBox::type_error(
            "WebRTC publication requires at least one render target",
        ));
    }
    if targets.iter().copied().collect::<HashSet<_>>().len() != targets.len() {
        return Err(JsErrorBox::type_error(
            "WebRTC render-target indices must be unique",
        ));
    }
    let host = state.borrow().borrow::<Arc<dyn PolymerHost>>().clone();
    host.webrtc_start(InstanceToken(token), bind, targets)
        .await
        .map_err(JsErrorBox::generic)
}

#[op2]
async fn op_webrtc_stop(state: Rc<RefCell<OpState>>, token: u32) -> Result<(), JsErrorBox> {
    let host = state.borrow().borrow::<Arc<dyn PolymerHost>>().clone();
    host.webrtc_stop(InstanceToken(token))
        .await
        .map_err(JsErrorBox::generic)
}

deno_core::extension!(
    polymer_api,
    ops = [
        op_scraping_package_from_path,
        op_configuration_describe,
        op_configuration_configure,
        op_configuration_prepare,
        op_plugin_list,
        op_plugin_invoke,
        op_prepared_filesystem_release,
        op_consolestorage_list,
        op_consolestorage_supports,
        op_consolestorage_create,
        op_consolestorage_delete,
        op_consolestorage_set_label,
        op_library_games_list,
        op_library_games_get,
        op_library_games_delete,
        op_library_launchables_add,
        op_library_launchables_delete,
        op_library_metadata_add,
        op_library_metadata_update,
        op_library_metadata_delete,
        op_library_metadata_replace,
        op_library_metadata_text,
        op_library_metadata_url,
        op_scraping_jobs, op_scraping_start, op_scraping_job, op_scraping_retry, op_scraping_scrapers,
        op_scraping_refresh,
        op_emulator_launch,
        op_emulator_close,
        op_emulator_set_volume,
        op_emulator_set_muted,
        op_emulator_message,
        op_emulator_input,
        op_emulator_pause_state,
        op_emulator_save_state, op_emulator_resume_state, op_emulator_state_screenshot,
        op_input_configure,
        op_controller_api_snapshot,
        op_controller_api_command,
        op_input_get,
        op_controller_device_command,
        op_webrtc_start,
        op_webrtc_stop,
        op_polymer_next_notification,
    ],
    esm_entry_point = "ext:polymer_api/99_main.js",
    esm = [dir "js",
        "00_infra.js",
        "01_scraping.js",
        "02_plugin.js",
        "03_configuration.js",
        "04_input.js",
        "05_webrtc.js",
        "06_emulator.js",
        "07_controllers.js",
        "08_consolestorage.js",
        "09_library.js",
        "99_main.js",
    ],
    options = {
        host: Arc<dyn PolymerHost>,
        notifications: PolymerNotificationInbox,
    },
    state = |state, options| {
        state.put(options.host);
        state.put(Rc::new(options.notifications));
    },
);

pub fn init(host: impl PolymerHost + 'static) -> (deno_core::Extension, PolymerNotificationSender) {
    let (sender, inbox) = notification_channel();
    (polymer_api::init(Arc::new(host), inbox), sender)
}

pub fn init_with_notifications<H: PolymerHost + 'static>(
    host: impl FnOnce(PolymerNotificationSender) -> H,
) -> (deno_core::Extension, PolymerNotificationSender) {
    let (sender, inbox) = notification_channel();
    let host = host(sender.clone());
    (polymer_api::init(Arc::new(host), inbox), sender)
}
