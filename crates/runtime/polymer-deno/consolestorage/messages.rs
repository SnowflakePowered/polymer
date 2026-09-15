use actix::{Message, Recipient};
use serde_json::Value;

/// Typed storage endpoints exposed to Deno without depending on the backend.
#[derive(Clone, Debug)]
pub struct ConsoleStorage {
    pub platforms: Recipient<Platforms>,
    pub inspect: Recipient<Inspect>,
    pub recover: Recipient<Recover>,
    pub preflight: Recipient<Preflight>,
    pub install: Recipient<Install>,
    pub bootstrap: Recipient<Bootstrap>,
    pub uninstall: Recipient<Uninstall>,
    pub plan_removal: Recipient<PlanRemoval>,
    pub list: Recipient<List>,
    pub supports: Recipient<Supports>,
    pub create: Recipient<Create>,
    pub delete: Recipient<Delete>,
    pub set_label: Recipient<SetLabel>,
}

#[derive(Debug, Message)]
#[rtype(result = "Result<Value, String>")]
pub struct Platforms;

#[derive(Debug, Message)]
#[rtype(result = "Result<Value, String>")]
pub struct Inspect {
    pub platform: String,
    pub id: String,
}

#[derive(Debug, Message)]
#[rtype(result = "Result<Value, String>")]
pub struct Recover {
    pub platform: String,
    pub id: String,
}

#[derive(Debug, Message)]
#[rtype(result = "Result<Value, String>")]
pub struct Preflight {
    pub platform: String,
    pub id: String,
    pub operation: String,
    pub settings: Value,
}

#[derive(Debug, Message)]
#[rtype(result = "Result<Value, String>")]
pub struct Install {
    pub platform: String,
    pub id: String,
    pub source: Value,
}

#[derive(Debug, Message)]
#[rtype(result = "Result<Value, String>")]
pub struct Bootstrap {
    pub platform: String,
    pub id: String,
    pub settings: Value,
}

#[derive(Debug, Message)]
#[rtype(result = "Result<Value, String>")]
pub struct Uninstall {
    pub platform: String,
    pub id: String,
    pub ids: Vec<String>,
    pub revision: String,
}

#[derive(Debug, Message)]
#[rtype(result = "Result<Value, String>")]
pub struct PlanRemoval {
    pub platform: String,
    pub id: String,
    pub ids: Vec<String>,
}

#[derive(Debug, Message)]
#[rtype(result = "Result<Value, String>")]
pub struct List;

#[derive(Debug, Message)]
#[rtype(result = "Result<Value, String>")]
pub struct Supports {
    pub kind: String,
    pub platform: String,
}

#[derive(Debug, Message)]
#[rtype(result = "Result<Value, String>")]
pub struct Create {
    pub kind: String,
    pub platform: String,
    pub label: Option<String>,
}

#[derive(Debug, Message)]
#[rtype(result = "Result<Value, String>")]
pub struct Delete {
    pub kind: String,
    pub platform: String,
    pub id: String,
}

#[derive(Debug, Message)]
#[rtype(result = "Result<Value, String>")]
pub struct SetLabel {
    pub kind: String,
    pub platform: String,
    pub id: String,
    pub label: Option<String>,
}
