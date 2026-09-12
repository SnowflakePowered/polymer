use polymer_deno::polymer::InstanceToken;
use serde_json::Value;
use std::time::Duration;

#[derive(actix::Message)]
#[rtype(result = "Result<(), String>")]
pub struct Launch {
    pub token: InstanceToken,
    pub options: Value,
    pub prepared_filesystem: Option<polymer_vfs::PreparedFilesystemLaunch>,
    pub controller_session: Option<polymer_pluginhost::controller::ControllerSession>,
}

#[derive(actix::Message)]
#[rtype(result = "()")]
pub struct Close {
    pub token: InstanceToken,
}

#[derive(actix::Message)]
#[rtype(result = "()")]
pub struct SetVolume {
    pub token: InstanceToken,
    pub volume: f64,
}

#[derive(actix::Message)]
#[rtype(result = "()")]
pub struct SetMuted {
    pub token: InstanceToken,
    pub muted: bool,
}

#[derive(actix::Message)]
#[rtype(result = "()")]
pub struct Message {
    pub token: InstanceToken,
    pub name: String,
    pub args: Value,
}

#[derive(actix::Message)]
#[rtype(result = "()")]
pub struct Input {
    pub token: InstanceToken,
    pub active: bool,
}

#[derive(actix::Message)]
#[rtype(result = "Result<(), String>")]
pub struct PauseState {
    pub token: InstanceToken,
    pub pause: bool,
    pub timeout: Duration,
}

#[derive(actix::Message)]
#[rtype(result = "Result<(), String>")]
pub struct ChangeDisc {
    pub token: InstanceToken,
    pub index: u32,
}

#[derive(actix::Message)]
#[rtype(result = "Result<Value, String>")]
pub struct SaveState {
    pub token: InstanceToken,
    pub timeout: Duration,
}

#[derive(actix::Message)]
#[rtype(result = "Result<Value, String>")]
pub struct ResumeState {
    pub token: InstanceToken,
    pub slot: u32,
    pub timeout: Duration,
}

#[derive(actix::Message)]
#[rtype(result = "Result<Option<Vec<u8>>, String>")]
pub struct StateScreenshot {
    pub token: InstanceToken,
    pub record_id: String,
}
