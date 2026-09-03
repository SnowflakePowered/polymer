use tokio::sync::mpsc;

use crate::{AutomationDriver, AutomationError, AutomationRequest, AutomationResponse};

// Cloneable asynchronous access to one process-scoped automation actor.
snowglass_command::command_client! {
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
        let (commands, mut receiver) = mpsc::unbounded_channel();
        std::thread::Builder::new()
            .name("snowglass-ui-automation".to_owned())
            .spawn(move || {
                while let Some(command) = receiver.blocking_recv() {
                    match command {
                        AutomationSessionCommand::Execute { request, response } => {
                            let _ = response.send(driver.execute(request));
                        }
                        AutomationSessionCommand::Stop {} => break,
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

impl Drop for AutomationSession {
    fn drop(&mut self) {
        if self.sender.is_unique() {
            self.stop();
        }
    }
}
