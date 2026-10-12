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
    future::{Future, poll_fn},
    pin::Pin,
    rc::Rc,
    sync::Arc,
    time::Duration,
};

#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformSettings {
    pub emulator: Option<String>,
}

#[path = "consolestorage/mod.rs"]
pub mod consolestorage;
pub use consolestorage::ConsoleStorage;
use consolestorage::{
    op_consolestorage_bootstrap, op_consolestorage_create, op_consolestorage_delete,
    op_consolestorage_fork, op_consolestorage_inspect, op_consolestorage_install,
    op_consolestorage_list, op_consolestorage_plan_removal, op_consolestorage_platforms,
    op_consolestorage_recover, op_consolestorage_save_copy, op_consolestorage_save_delete,
    op_consolestorage_save_history, op_consolestorage_save_restore, op_consolestorage_saves,
    op_consolestorage_set_label, op_consolestorage_supports, op_consolestorage_uninstall,
};

#[path = "jobs/mod.rs"]
pub mod jobs;
pub use jobs::Jobs;
use jobs::{
    op_jobs_cancel, op_jobs_check_updates, op_jobs_dismiss, op_jobs_fetch_update, op_jobs_get,
    op_jobs_import, op_jobs_list, op_jobs_retry,
};

#[path = "library/mod.rs"]
pub mod library;

pub use library::Library;
use library::{
    op_library_games_delete, op_library_games_get, op_library_games_list,
    op_library_games_platforms, op_library_import_key, op_library_keys_list,
    op_library_keys_licensed, op_library_keys_platforms, op_library_keys_remove,
    op_library_launchables_add,
    op_library_launchables_delete, op_library_launchables_find, op_library_metadata_add,
    op_library_metadata_delete, op_library_metadata_replace, op_library_metadata_text,
    op_library_metadata_update, op_library_metadata_url, op_scraping_refresh, op_scraping_scrapers,
    op_scraping_start,
};

pub type ApiFuture<T> = Pin<Box<dyn Future<Output = T> + 'static>>;

pub(crate) trait ServiceError: std::fmt::Display {
    fn unavailable(error: actix::MailboxError) -> JsErrorBox;

    fn into_js(self) -> JsErrorBox
    where
        Self: Sized,
    {
        JsErrorBox::generic(self.to_string())
    }
}

pub(crate) trait ServiceCall<M, T> {
    async fn call(&self, message: M) -> Result<T, JsErrorBox>;
}

impl<M, T, E> ServiceCall<M, T> for actix::Recipient<M>
where
    M: actix::Message<Result = Result<T, E>> + Send + 'static,
    T: Send + 'static,
    E: ServiceError + Send + 'static,
{
    async fn call(&self, message: M) -> Result<T, JsErrorBox> {
        self.send(message)
            .await
            .map_err(E::unavailable)?
            .map_err(ServiceError::into_js)
    }
}

/// Native capabilities exposed to the application-facing Polymer realm.
///
/// This is the only native interface understood by the frontend extension.
/// Implementations own lifecycle, storage, configuration, and plugin policy;
/// ops only validate their JavaScript inputs and delegate here.
pub trait PolymerHost: Send + Sync {
    fn scrape(&self, path: String) -> ApiFuture<Result<Value, String>>;
    fn digest(
        &self,
        uri: String,
        binary: bool,
        algorithm: String,
    ) -> ApiFuture<Result<String, String>>;
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
    fn select_emulator(
        &self,
        launchable_id: String,
        emulator: Option<String>,
    ) -> ApiFuture<Result<(), String>>;
    fn platform_settings(&self) -> ApiFuture<Result<Value, String>>;
    fn update_platform_settings(
        &self,
        platform: String,
        settings: PlatformSettings,
    ) -> ApiFuture<Result<Value, String>>;
    fn list_plugins(&self) -> Value;
    fn invoke_plugin(
        &self,
        plugin: String,
        path: Vec<String>,
        arguments: Vec<Value>,
    ) -> ApiFuture<Value>;
    fn release_prepared_filesystem(&self, token: String) -> bool;
    fn consolestorage(&self) -> Result<ConsoleStorage, String>;
    fn library(&self) -> Result<Library, String>;
    fn jobs(&self) -> Result<Jobs, String>;
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
    fn emulator_change_disc(
        &self,
        token: InstanceToken,
        index: u32,
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
    JobChanged(Value),
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
async fn op_configuration_select(
    state: Rc<RefCell<OpState>>,
    #[string] launchable_id: String,
    #[string] emulator: Option<String>,
) -> Result<(), JsErrorBox> {
    let host = state.borrow().borrow::<Arc<dyn PolymerHost>>().clone();
    host.select_emulator(launchable_id, emulator)
        .await
        .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
async fn op_configuration_platforms(
    state: Rc<RefCell<OpState>>,
) -> Result<serde_json::Value, JsErrorBox> {
    let host = state.borrow().borrow::<Arc<dyn PolymerHost>>().clone();
    host.platform_settings().await.map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
async fn op_configuration_update_platform(
    state: Rc<RefCell<OpState>>,
    #[string] platform: String,
    #[serde] settings: PlatformSettings,
) -> Result<serde_json::Value, JsErrorBox> {
    let host = state.borrow().borrow::<Arc<dyn PolymerHost>>().clone();
    host.update_platform_settings(platform, settings)
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
fn op_machine(#[string] id: String) -> Option<consolespec::MachineSpec> {
    consolespec::MachineSpec::try_from(id.as_str()).ok()
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
async fn op_emulator_change_disc(
    state: Rc<RefCell<OpState>>,
    token: u32,
    index: u32,
) -> Result<(), JsErrorBox> {
    let host = state.borrow().borrow::<Arc<dyn PolymerHost>>().clone();
    host.emulator_change_disc(InstanceToken(token), index)
        .await
        .map_err(JsErrorBox::generic)
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

deno_core::extension!(
    polymer_api,
    ops = [
        op_machine,
        op_scraping_package_from_path,
        op_configuration_describe,
        op_configuration_configure,
        op_configuration_prepare,
        op_configuration_select,
        op_configuration_platforms,
        op_configuration_update_platform,
        op_plugin_list,
        op_plugin_invoke,
        op_prepared_filesystem_release,
        op_consolestorage_platforms,
        op_consolestorage_inspect,
        op_consolestorage_recover,

        op_consolestorage_install,
        op_consolestorage_bootstrap,
        op_consolestorage_uninstall,
        op_consolestorage_plan_removal,
        op_consolestorage_list,
        op_consolestorage_supports,
        op_consolestorage_create,
        op_consolestorage_delete,
        op_consolestorage_set_label,
        op_consolestorage_fork,
        op_consolestorage_saves,
        op_consolestorage_save_history,
        op_consolestorage_save_restore,
        op_consolestorage_save_copy,
        op_consolestorage_save_delete,
        op_library_games_list,
        op_library_games_platforms,
        op_library_games_get,
        op_library_games_delete,
        op_library_import_key,
        op_library_keys_list,
        op_library_keys_platforms,
        op_library_keys_licensed,
        op_library_keys_remove,
        op_library_launchables_add,
        op_library_launchables_find,
        op_library_launchables_delete,
        op_library_metadata_add,
        op_library_metadata_update,
        op_library_metadata_delete,
        op_library_metadata_replace,
        op_library_metadata_text,
        op_library_metadata_url,
        op_scraping_start, op_scraping_scrapers, op_scraping_refresh,
        op_jobs_list, op_jobs_get, op_jobs_cancel, op_jobs_retry, op_jobs_dismiss, op_jobs_import, op_jobs_check_updates, op_jobs_fetch_update,
        op_emulator_launch,
        op_emulator_close,
        op_emulator_set_volume,
        op_emulator_set_muted,
        op_emulator_message,
        op_emulator_input,
        op_emulator_pause_state,
        op_emulator_change_disc,
        op_emulator_save_state, op_emulator_resume_state, op_emulator_state_screenshot,
        op_input_configure,
        op_controller_api_snapshot,
        op_controller_api_command,
        op_input_get,
        op_controller_device_command,
        op_polymer_next_notification,
    ],
    esm_entry_point = "ext:polymer_api/99_main.js",
    esm = [dir "js",
        "00_infra.js",
        "00_package.js",
        "01_scraping.js",
        "02_plugin.js",
        "03_configuration.js",
        "04_input.js",
        "06_emulator.js",
        "07_controllers.js",
        "08_consolestorage.js",
        "09_library.js",
        "10_jobs.js",
        "99_main.js",
    ],
    options = {
        host: Arc<dyn PolymerHost>,
        notifications: PolymerNotificationInbox,
    },
    state = |state, options| {
        state.put(crate::digest::DigestHandler(|state, source, algorithm| {
            let crate::digest::DigestSource::Uri { uri, binary } = source else {
                return Err(JsErrorBox::type_error("invalid frontend digest source"));
            };
            let host = state.borrow::<Arc<dyn PolymerHost>>().clone();
            Ok(Box::pin(async move {
                host.digest(uri, binary, algorithm)
                    .await
                    .map_err(JsErrorBox::generic)
            }))
        }));
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
