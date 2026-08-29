//! Process-scoped UI automation protocol and blocking-driver actor.

#![forbid(unsafe_code)]

mod driver;
mod protocol;
mod session;

pub use driver::AutomationDriver;
pub use protocol::{
    AutomationError, AutomationRequest, AutomationResponse, Bounds, Diagnosis, EventFilter,
    InputCommand, LocatorAction, NodeSnapshot, TextMatch, TreeOptions, UiEvent, WaitCondition,
    WindowId, WindowQuery, WindowSnapshot,
};
pub use session::AutomationSession;
