use kameo::{
    Actor,
    actor::{ActorRef, Spawn, WeakActorRef},
    error::{ActorStopReason, Infallible, SendError},
    message::{Context, Message},
};

use crate::{AutomationDriver, AutomationError, AutomationRequest, AutomationResponse};

pub type AutomationClient = ActorRef<AutomationActor>;

pub struct AutomationActor {
    driver: Option<Box<dyn AutomationDriver>>,
}

/// Owns the process-scoped worker independently of cloned actor references.
pub struct AutomationSession {
    actor: AutomationClient,
}

impl AutomationSession {
    pub fn spawn(driver: impl AutomationDriver) -> Result<Self, AutomationError> {
        // Own the runtime so synchronous and current-thread callers work too.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| AutomationError::platform(error.to_string()))?;
        let prepared = AutomationActor::prepare();
        let actor = prepared.actor_ref().clone();
        std::thread::Builder::new()
            .name("polymer-ui-automation".to_owned())
            .spawn(move || {
                // The actor and all platform resources are dropped on this thread.
                let _ = runtime.block_on(prepared.run(Box::new(driver)));
            })
            .map_err(|error| AutomationError::platform(error.to_string()))?;
        Ok(Self { actor })
    }

    pub fn client(&self) -> AutomationClient {
        self.actor.clone()
    }

    pub fn close(&self) {
        // Discard queued work when the process closes; on_stop still runs.
        self.actor.kill();
    }

    pub async fn shutdown(&self) -> Result<(), AutomationError> {
        self.close();
        self.actor
            .wait_for_shutdown_with_result(|result| {
                result
                    .map(|_| ())
                    .map_err(|error| AutomationError::platform(error.to_string()))
            })
            .await
    }
}

impl Drop for AutomationSession {
    fn drop(&mut self) {
        self.close();
    }
}

impl Actor for AutomationActor {
    type Args = Box<dyn AutomationDriver>;
    type Error = Infallible;

    async fn on_start(driver: Self::Args, _: ActorRef<Self>) -> Result<Self, Self::Error> {
        Ok(Self {
            driver: Some(driver),
        })
    }

    async fn on_stop(
        &mut self,
        _: WeakActorRef<Self>,
        _: ActorStopReason,
    ) -> Result<(), Self::Error> {
        if let Some(mut driver) = self.driver.take() {
            driver.shutdown();
        }
        Ok(())
    }
}

impl Message<AutomationRequest> for AutomationActor {
    type Reply = Result<AutomationResponse, AutomationError>;

    async fn handle(
        &mut self,
        request: AutomationRequest,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        // Observe shutdown between blocking calls, even with a ready mailbox.
        tokio::task::yield_now().await;
        self.driver
            .as_mut()
            .expect("automation driver is running")
            .execute(request)
    }
}

impl From<SendError<AutomationRequest, AutomationError>> for AutomationError {
    fn from(error: SendError<AutomationRequest, AutomationError>) -> Self {
        match error {
            SendError::HandlerError(error) => error,
            _ => Self::process_exited(),
        }
    }
}

#[cfg(test)]
mod tests;
