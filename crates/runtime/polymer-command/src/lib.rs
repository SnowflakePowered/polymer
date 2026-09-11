//! Typed Kameo clients for native services and direct owner-thread mailboxes.

#![forbid(unsafe_code)]

mod lifecycle;
mod local;
mod protocol;
mod request;

pub use lifecycle::{ExecutorLifetime, WorkerOwner};

pub use local::{Completion, Deliver, LocalDelivery};
pub use request::{Admission, PendingReply, Queued, ReplyClosed};

#[doc(hidden)]
pub use kameo as __kameo;

use std::{future::Future, pin::Pin};

pub use polymer_command_macros::{command_handler, native_handler};

#[doc(hidden)]
pub use paste::paste as __paste;
#[doc(hidden)]
pub use tokio as __tokio;
#[doc(hidden)]
pub use tracing as __tracing;

pub type CommandFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionFailure {
    Closed,
    Timeout,
}

#[doc(hidden)]
pub use futures::executor::block_on as __block_on;
