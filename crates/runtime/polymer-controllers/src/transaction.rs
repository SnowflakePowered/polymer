use crate::ControllerTopology;
use std::time::Duration;

pub const DEFAULT_CONTROLLER_TOPOLOGY_TRANSACTION_TIMEOUT: Duration = Duration::from_secs(60);
pub const CONTROLLER_SESSION_COMMIT_COMMAND: &str = "controllers.session.commit";

#[derive(Debug)]
pub struct ControllerTopologyChange {
    pub previous: ControllerTopology,
    pub requested: ControllerTopology,
}
