use polymer_deno::polymer::InstanceToken;

#[derive(actix::Message)]
#[rtype(result = "Result<(), String>")]
pub struct Start {
    pub token: InstanceToken,
    pub bind: std::net::SocketAddr,
    pub targets: Vec<u32>,
}

#[derive(actix::Message)]
#[rtype(result = "Result<(), String>")]
pub struct Stop {
    pub token: InstanceToken,
}
