//! Optional user-facing Snowglass APIs for a host-owned Deno isolate.
//!
//! The base [`crate::Runtime`] deliberately does not install this extension.
//! Plugin workers therefore retain the curated web platform without gaining
//! access to the application-facing `Snowglass` global.

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
};

pub type ApiFuture<T> = Pin<Box<dyn Future<Output = T> + 'static>>;

/// Native capabilities exposed to the application-facing Snowglass realm.
///
/// This is the only native interface understood by the frontend extension.
/// Implementations own lifecycle, storage, configuration, and plugin policy;
/// ops only validate their JavaScript inputs and delegate here.
pub trait SnowglassHost: Send + Sync {
    fn scrape(&self, path: String) -> ApiFuture<Result<Value, String>>;
    fn describe_configuration(
        &self,
        package: Value,
        preferred_emulator: Option<String>,
    ) -> Result<Value, String>;
    fn prepare_configuration(&self, package: Value, request: Value) -> Result<Value, String>;
    fn list_plugins(&self) -> Value;
    fn invoke_plugin(
        &self,
        plugin: String,
        path: Vec<String>,
        arguments: Vec<Value>,
    ) -> ApiFuture<Value>;
    fn release_prepared_filesystem(&self, token: String) -> bool;
    fn consolestorage(&self, request: ConsoleStorageRequest) -> ApiFuture<Result<Value, String>>;
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
    fn input_configure(&self, request_id: u32, transaction: Value);
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
pub enum SnowglassNotification {
    OpenRequested(Value),
    DropActiveChanged(bool),
    InputConfigurationChanged {
        configuration: Value,
        layouts: Value,
    },
    InputStateChanged {
        sources: Value,
        priority: String,
        controls: Value,
    },
    InputConfigurationSettled {
        request_id: u64,
        result: Value,
    },
    EmulatorInstance(Value),
    GameSaved {
        kind: String,
        platform: String,
        id: String,
    },
}

/// Cloneable native-to-JavaScript notification endpoint.
///
/// The receiver lives entirely inside the Deno extension. Native feature
/// producers retain this sender instead of reaching through the shell's DOM
/// and runtime ownership layers.
pub type SnowglassNotificationSender = tokio::sync::mpsc::UnboundedSender<SnowglassNotification>;

struct SnowglassNotificationInbox {
    receiver: RefCell<tokio::sync::mpsc::UnboundedReceiver<SnowglassNotification>>,
}

fn notification_channel() -> (SnowglassNotificationSender, SnowglassNotificationInbox) {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    (
        sender,
        SnowglassNotificationInbox {
            receiver: RefCell::new(receiver),
        },
    )
}

#[op2]
#[serde]
async fn op_snowglass_next_notification(
    state: Rc<RefCell<OpState>>,
) -> Option<SnowglassNotification> {
    let inbox = state
        .borrow()
        .borrow::<Rc<SnowglassNotificationInbox>>()
        .clone();
    poll_fn(|context| inbox.receiver.borrow_mut().poll_recv(context)).await
}

#[op2]
#[serde]
async fn op_scraping_package_from_path(
    state: Rc<RefCell<OpState>>,
    #[string] path: String,
) -> Result<serde_json::Value, JsErrorBox> {
    let host = state.borrow().borrow::<Arc<dyn SnowglassHost>>().clone();
    host.scrape(path).await.map_err(JsErrorBox::generic)
}

async fn consolestorage(
    state: &Rc<RefCell<OpState>>,
    request: ConsoleStorageRequest,
) -> Result<serde_json::Value, JsErrorBox> {
    let host = state.borrow().borrow::<Arc<dyn SnowglassHost>>().clone();
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
fn op_configuration_describe(
    state: &mut OpState,
    #[serde] package: serde_json::Value,
    #[string] preferred_emulator: Option<String>,
) -> Result<serde_json::Value, JsErrorBox> {
    state
        .borrow::<Arc<dyn SnowglassHost>>()
        .describe_configuration(package, preferred_emulator)
        .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
fn op_configuration_prepare(
    state: &mut OpState,
    #[serde] package: serde_json::Value,
    #[serde] request: serde_json::Value,
) -> Result<serde_json::Value, JsErrorBox> {
    state
        .borrow::<Arc<dyn SnowglassHost>>()
        .prepare_configuration(package, request)
        .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
fn op_plugin_list(state: &mut OpState) -> Result<serde_json::Value, JsErrorBox> {
    Ok(state.borrow::<Arc<dyn SnowglassHost>>().list_plugins())
}

#[op2]
#[serde]
async fn op_plugin_invoke(
    state: Rc<RefCell<OpState>>,
    #[string] plugin: String,
    #[serde] path: Vec<String>,
    #[serde] arguments: Vec<serde_json::Value>,
) -> serde_json::Value {
    let host = state.borrow().borrow::<Arc<dyn SnowglassHost>>().clone();
    host.invoke_plugin(plugin, path, arguments).await
}

#[op2(fast)]
fn op_prepared_filesystem_release(state: &mut OpState, #[string] token: String) -> bool {
    state
        .borrow::<Arc<dyn SnowglassHost>>()
        .release_prepared_filesystem(token)
}

#[op2]
async fn op_emulator_launch(
    state: Rc<RefCell<OpState>>,
    token: u32,
    #[serde] options: serde_json::Value,
) -> Result<(), JsErrorBox> {
    let host = state.borrow().borrow::<Arc<dyn SnowglassHost>>().clone();
    host.emulator_launch(InstanceToken(token), options)
        .await
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
fn op_emulator_close(state: &mut OpState, token: u32) {
    state
        .borrow::<Arc<dyn SnowglassHost>>()
        .emulator_close(InstanceToken(token));
}

#[op2(fast)]
fn op_emulator_set_volume(state: &mut OpState, token: u32, volume: f64) {
    state
        .borrow::<Arc<dyn SnowglassHost>>()
        .emulator_set_volume(InstanceToken(token), volume);
}

#[op2(fast)]
fn op_emulator_set_muted(state: &mut OpState, token: u32, muted: bool) {
    state
        .borrow::<Arc<dyn SnowglassHost>>()
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
        .borrow::<Arc<dyn SnowglassHost>>()
        .emulator_message(InstanceToken(token), name, args);
}

#[op2(fast)]
fn op_emulator_input(state: &mut OpState, token: u32, active: bool) {
    state
        .borrow::<Arc<dyn SnowglassHost>>()
        .emulator_input(InstanceToken(token), active);
}

#[op2]
fn op_input_configure(
    state: &mut OpState,
    request_id: u32,
    #[serde] transaction: serde_json::Value,
) {
    state
        .borrow::<Arc<dyn SnowglassHost>>()
        .input_configure(request_id, transaction);
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
    let host = state.borrow().borrow::<Arc<dyn SnowglassHost>>().clone();
    host.webrtc_start(InstanceToken(token), bind, targets)
        .await
        .map_err(JsErrorBox::generic)
}

#[op2]
async fn op_webrtc_stop(state: Rc<RefCell<OpState>>, token: u32) -> Result<(), JsErrorBox> {
    let host = state.borrow().borrow::<Arc<dyn SnowglassHost>>().clone();
    host.webrtc_stop(InstanceToken(token))
        .await
        .map_err(JsErrorBox::generic)
}

deno_core::extension!(
    snowglass_api,
    ops = [
        op_scraping_package_from_path,
        op_configuration_describe,
        op_configuration_prepare,
        op_plugin_list,
        op_plugin_invoke,
        op_prepared_filesystem_release,
        op_consolestorage_list,
        op_consolestorage_create,
        op_consolestorage_delete,
        op_consolestorage_set_label,
        op_emulator_launch,
        op_emulator_close,
        op_emulator_set_volume,
        op_emulator_set_muted,
        op_emulator_message,
        op_emulator_input,
        op_input_configure,
        op_webrtc_start,
        op_webrtc_stop,
        op_snowglass_next_notification,
    ],
    esm_entry_point = "ext:snowglass_api/99_main.js",
    esm = [dir "js",
        "00_infra.js",
        "01_scraping.js",
        "02_plugin.js",
        "03_configuration.js",
        "04_input.js",
        "05_webrtc.js",
        "06_emulator.js",
        "08_consolestorage.js",
        "99_main.js",
    ],
    options = {
        host: Arc<dyn SnowglassHost>,
        notifications: SnowglassNotificationInbox,
    },
    state = |state, options| {
        state.put(options.host);
        state.put(Rc::new(options.notifications));
    },
);

pub fn init(
    host: impl SnowglassHost + 'static,
) -> (deno_core::Extension, SnowglassNotificationSender) {
    let (sender, inbox) = notification_channel();
    (snowglass_api::init(Arc::new(host), inbox), sender)
}

pub fn init_with_notifications<H: SnowglassHost + 'static>(
    host: impl FnOnce(SnowglassNotificationSender) -> H,
) -> (deno_core::Extension, SnowglassNotificationSender) {
    let (sender, inbox) = notification_channel();
    let host = host(sender.clone());
    (snowglass_api::init(Arc::new(host), inbox), sender)
}
