//! Typed direct mailboxes for thread-owned executors.

#![forbid(unsafe_code)]

mod lifecycle;
mod local;
mod protocol;
mod request;

pub use lifecycle::{ExecutorLifetime, WorkerOwner};

pub use local::{Completion, Deliver, LocalDelivery};
pub use request::{Admission, PendingReply, ReplyClosed};

use std::{future::Future, pin::Pin};

pub use polymer_command_macros::command_handler;

#[doc(hidden)]
pub use paste::paste as __paste;
#[doc(hidden)]
pub use tracing as __tracing;

pub type CommandFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

#[doc(hidden)]
pub use futures::executor::block_on as __block_on;
