//! Snowglass IPC transports.

pub mod control;
pub mod frame;
pub mod input;
pub mod ui;

mod shared;

pub use control::{LocalSocket, LocalSocketListener, Message, StreamReady};

/// A synchronous producer for messages carried by the existing input IPC
/// connection. Implementations should enqueue directly and must not add a
/// second forwarding worker.
pub trait MessageSink: Send + Sync + 'static {
    fn send_message(&self, message: Message) -> bool;
}

impl<T: MessageSink + ?Sized> MessageSink for std::sync::Arc<T> {
    fn send_message(&self, message: Message) -> bool {
        (**self).send_message(message)
    }
}
