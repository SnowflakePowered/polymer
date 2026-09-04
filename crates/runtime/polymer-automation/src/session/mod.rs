use tokio::sync::mpsc;

use crate::{AutomationDriver, AutomationError, AutomationRequest, AutomationResponse};
use polymer_command::command_handler;

// Cloneable asynchronous access to one process-scoped automation actor.
polymer_command::command_client! {
    pub client AutomationSession {
        closed: AutomationError::process_exited();

        #[args(Execute)]
        pub async fn execute(
            request: AutomationRequest,
        ) -> Result<AutomationResponse, AutomationError>;

        #[oneshot]
        #[args(Stop)]
        fn stop();
    }
}

impl AutomationSession {
    pub fn spawn(mut driver: impl AutomationDriver) -> Result<Self, AutomationError> {
        let (commands, mut receiver) = mpsc::unbounded_channel::<AutomationSessionCommand>();
        std::thread::Builder::new()
            .name("polymer-ui-automation".to_owned())
            .spawn(move || {
                while let Some(command) = receiver.blocking_recv() {
                    if command.dispatch(&mut driver) {
                        break;
                    }
                }
                driver.shutdown();
            })
            .map_err(|error| AutomationError::platform(error.to_string()))?;
        Ok(Self::new(commands))
    }

    pub fn close(&self) {
        self.stop();
    }
}

#[command_handler]
impl<Driver: AutomationDriver> AutomationSessionCommandHandler for Driver {
    type Output<'handler>
        = bool
    where
        Self: 'handler;

    fn execute(
        &mut self,
        request: AutomationRequest,
    ) -> Result<AutomationResponse, AutomationError> {
        AutomationDriver::execute(self, request)
    }

    fn stop<'handler>(&'handler mut self) -> Self::Output<'handler> {
        true
    }
}

impl Drop for AutomationSession {
    fn drop(&mut self) {
        if self.sender.is_unique() {
            self.stop();
        }
    }
}
