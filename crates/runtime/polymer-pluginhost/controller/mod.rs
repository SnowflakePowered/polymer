mod messages;

pub use crate::runtime::controller_session::ControllerActor;
pub use messages::*;
use std::sync::Arc;

pub type ControllerClient = actix::Addr<ControllerActor>;
pub type PendingReply<M> = std::pin::Pin<Box<actix::prelude::Request<ControllerActor, M>>>;

/// Owns the isolate lifetime; cloned addresses only submit requests.
pub struct ControllerSession {
    pub(crate) client: ControllerClient,
    pub(crate) control: Arc<crate::runtime::InvocationControl>,
    pub(crate) thread: Option<std::thread::JoinHandle<Result<(), String>>>,
}

impl ControllerSession {
    pub fn client(&self) -> ControllerClient {
        self.client.clone()
    }

    pub fn close(&self) {
        // V8 termination is out of band so even synchronous JavaScript can be stopped.
        self.control.cancel();
    }

    /// Wait for the actor, isolate, and owning thread to finish cleanup.
    pub async fn shutdown(mut self) -> Result<(), String> {
        self.close();
        if let Some(thread) = self.thread.take() {
            tokio::task::spawn_blocking(move || thread.join())
                .await
                .map_err(|error| error.to_string())?
                .map_err(|_| "controller actor thread panicked".to_owned())??;
        }
        Ok(())
    }
}

impl std::fmt::Debug for ControllerSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControllerSession")
            .field("actor", &self.client)
            .finish_non_exhaustive()
    }
}

impl Drop for ControllerSession {
    fn drop(&mut self) {
        self.close();
    }
}

impl std::ops::Deref for ControllerSession {
    type Target = ControllerClient;
    fn deref(&self) -> &ControllerClient {
        &self.client
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PauseState {
    Pause,
    Resume,
}

impl PauseState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Pause => "pause",
            Self::Resume => "resume",
        }
    }
}

deno_core::extension!(
    polymer_controller,
    esm_entry_point = "ext:polymer_controller/00_controller.js",
    esm = [dir "controller/js", "00_controller.js"],
);

pub(crate) fn extension() -> deno_core::Extension {
    polymer_controller::init()
}
