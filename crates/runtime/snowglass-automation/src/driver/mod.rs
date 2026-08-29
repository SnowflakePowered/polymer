use crate::{AutomationError, AutomationRequest, AutomationResponse};

/// Blocking platform implementation owned by an [`AutomationSession`](crate::AutomationSession).
pub trait AutomationDriver: Send + 'static {
    fn execute(
        &mut self,
        request: AutomationRequest,
    ) -> Result<AutomationResponse, AutomationError>;

    fn shutdown(&mut self) {}
}
