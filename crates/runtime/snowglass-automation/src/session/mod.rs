use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};

use crate::{AutomationDriver, AutomationError, AutomationRequest, AutomationResponse};

enum SessionCommand {
    Execute {
        request: AutomationRequest,
        response: oneshot::Sender<Result<AutomationResponse, AutomationError>>,
    },
    Stop,
}

struct SessionState {
    commands: mpsc::UnboundedSender<SessionCommand>,
}

/// Cloneable asynchronous access to one process-scoped automation actor.
#[derive(Clone)]
pub struct AutomationSession {
    state: Arc<SessionState>,
}

impl AutomationSession {
    pub fn spawn(mut driver: impl AutomationDriver) -> Result<Self, AutomationError> {
        let (commands, mut receiver) = mpsc::unbounded_channel();
        std::thread::Builder::new()
            .name("snowglass-ui-automation".to_owned())
            .spawn(move || {
                while let Some(command) = receiver.blocking_recv() {
                    match command {
                        SessionCommand::Execute { request, response } => {
                            let _ = response.send(driver.execute(request));
                        }
                        SessionCommand::Stop => break,
                    }
                }
                driver.shutdown();
            })
            .map_err(|error| AutomationError::platform(error.to_string()))?;
        Ok(Self {
            state: Arc::new(SessionState { commands }),
        })
    }

    pub async fn execute(
        &self,
        request: AutomationRequest,
    ) -> Result<AutomationResponse, AutomationError> {
        let (response, receiver) = oneshot::channel();
        self.state
            .commands
            .send(SessionCommand::Execute { request, response })
            .map_err(|_| AutomationError::process_exited())?;
        receiver
            .await
            .map_err(|_| AutomationError::process_exited())?
    }

    pub fn close(&self) {
        let _ = self.state.commands.send(SessionCommand::Stop);
    }
}

impl Drop for SessionState {
    fn drop(&mut self) {
        let _ = self.commands.send(SessionCommand::Stop);
    }
}
