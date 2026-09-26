use actix::{Actor, ActorContext, Addr, Context, Handler, System};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::watch;

use crate::{
    AutomationDriver, AutomationError, AutomationRequest, AutomationResponse, MessageBoxes,
    WatchMessageBoxes,
};

pub type AutomationClient = Addr<AutomationActor>;

pub struct AutomationActor {
    driver: Box<dyn AutomationDriver>,
    closing: Arc<AtomicBool>,
}

/// Owns the process-scoped worker independently of cloned actor addresses.
pub struct AutomationSession {
    actor: AutomationClient,
    system: System,
    closing: Arc<AtomicBool>,
    finished: watch::Receiver<Option<Result<(), AutomationError>>>,
}

impl AutomationSession {
    pub fn spawn(driver: impl AutomationDriver) -> Result<Self, AutomationError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| AutomationError::platform(error.to_string()))?;
        let (ready, startup) = std::sync::mpsc::sync_channel(1);
        let (completed, finished) = watch::channel(None);
        let closing = Arc::new(AtomicBool::new(false));
        let actor_closing = closing.clone();
        std::thread::Builder::new()
            .name("polymer-ui-automation".to_owned())
            .spawn(move || {
                let runner = System::with_tokio_rt(|| runtime);
                let actor = runner.block_on(async move {
                    AutomationActor::create(move |ctx| {
                        ctx.set_mailbox_capacity(0);
                        AutomationActor {
                            driver: Box::new(driver),
                            closing: actor_closing,
                        }
                    })
                });
                let result = if ready.send((actor, System::current())).is_ok() {
                    runner
                        .run()
                        .map_err(|error| AutomationError::platform(error.to_string()))
                } else {
                    drop(runner);
                    Ok(())
                };
                // SystemRunner has dropped the actor and platform resources on this thread.
                let _ = completed.send(Some(result));
            })
            .map_err(|error| AutomationError::platform(error.to_string()))?;
        let (actor, system) = startup
            .recv()
            .map_err(|_| AutomationError::process_exited())?;
        Ok(Self {
            actor,
            system,
            closing,
            finished,
        })
    }

    pub fn client(&self) -> AutomationClient {
        self.actor.clone()
    }

    pub fn close(&self) {
        // Observe closure between blocking calls even if the mailbox stays ready.
        self.closing.store(true, Ordering::Release);
        self.system.stop();
    }

    pub async fn shutdown(&self) -> Result<(), AutomationError> {
        self.close();
        let mut finished = self.finished.clone();
        let result = finished.wait_for(Option::is_some).await.map_err(|_| {
            AutomationError::platform("UI automation worker terminated before cleanup completed")
        })?;
        result
            .as_ref()
            .expect("worker completion checked above")
            .clone()
    }
}

impl Drop for AutomationSession {
    fn drop(&mut self) {
        self.close();
    }
}

impl Actor for AutomationActor {
    type Context = Context<Self>;
    fn stopped(&mut self, _: &mut Self::Context) {
        System::current().stop();
    }
}

impl Drop for AutomationActor {
    fn drop(&mut self) {
        self.driver.shutdown();
    }
}

impl Handler<AutomationRequest> for AutomationActor {
    type Result = Result<AutomationResponse, AutomationError>;
    fn handle(&mut self, request: AutomationRequest, ctx: &mut Context<Self>) -> Self::Result {
        if self.closing.load(Ordering::Acquire) {
            ctx.stop();
            return Err(AutomationError::process_exited());
        }
        self.driver.execute(request)
    }
}

impl Handler<WatchMessageBoxes> for AutomationActor {
    type Result = Result<MessageBoxes, AutomationError>;
    fn handle(&mut self, _: WatchMessageBoxes, ctx: &mut Context<Self>) -> Self::Result {
        if self.closing.load(Ordering::Acquire) {
            ctx.stop();
            return Err(AutomationError::process_exited());
        }
        self.driver.message_boxes()
    }
}

impl From<actix::MailboxError> for AutomationError {
    fn from(_: actix::MailboxError) -> Self {
        Self::process_exited()
    }
}
