use crate::{AutomationError, AutomationRequest, AutomationResponse, MessageBoxes};

/// Blocking platform implementation owned by an [`AutomationSession`](crate::AutomationSession).
pub trait AutomationDriver: Send + 'static {
    fn execute(
        &mut self,
        request: AutomationRequest,
    ) -> Result<AutomationResponse, AutomationError>;

    fn message_boxes(&mut self) -> Result<MessageBoxes, AutomationError>;

    fn shutdown(&mut self) {}
}
