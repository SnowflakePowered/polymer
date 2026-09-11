use serde_json::Value;

#[derive(actix::Message)]
#[rtype(result = "()")]
pub struct Configure {
    pub request_id: u32,
    pub transaction: Value,
}

#[derive(actix::Message)]
#[rtype(result = "Result<Value, String>")]
pub struct ControllerApi {
    pub command: Value,
}
