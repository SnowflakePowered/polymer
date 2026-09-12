use actix::Message;
use std::time::Duration;

#[derive(Message)]
#[rtype(result = "Result<(), String>")]
pub struct AttachAutomation(pub polymer_automation::AutomationClient);

#[derive(Message)]
#[rtype(result = "Result<(), String>")]
pub struct AttachActions(pub crate::actions::ActionSink);

#[derive(Message)]
#[rtype(result = "Result<polymer_controllers::ControllerTopology, String>")]
pub struct ControllerChanged(pub polymer_controllers::ControllerTopologyChange);

#[derive(Message)]
#[rtype(result = "Result<(), String>")]
pub struct PauseResume {
    pub state: super::PauseState,
    pub timeout: Duration,
}

#[derive(Message)]
#[rtype(result = "Result<(), String>")]
pub struct DiscChange {
    pub index: u32,
    pub timeout: Duration,
}

#[derive(Message)]
#[rtype(result = "Result<(), String>")]
pub struct StateAction {
    pub slot: u32,
    pub save: bool,
    pub timeout: Duration,
}

#[derive(Message)]
#[rtype(result = "Result<Option<Vec<u8>>, String>")]
pub struct Quit {
    pub timeout: Duration,
}
