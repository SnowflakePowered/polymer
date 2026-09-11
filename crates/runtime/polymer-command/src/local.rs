use crate::{ExecutorLifetime, PendingReply, ReplyClosed};
use std::{fmt, sync::Arc};
use tokio::sync::{OwnedSemaphorePermit, oneshot};

/// Transfers ownership to a local executor, returning the envelope on failure.
pub trait Deliver<C>: Send + Sync + 'static {
    fn deliver(&self, command: C) -> Result<(), C>;
}

impl<C: Send + 'static> Deliver<C> for std::sync::mpsc::Sender<C> {
    fn deliver(&self, command: C) -> Result<(), C> {
        self.send(command).map_err(|error| error.0)
    }
}

impl<C: Send + 'static> Deliver<C> for tokio::sync::mpsc::UnboundedSender<C> {
    fn deliver(&self, command: C) -> Result<(), C> {
        self.send(command).map_err(|error| error.0)
    }
}

struct ProxyDelivery<F>(F);

impl<C, F: Fn(C) -> Result<(), C> + Send + Sync + 'static> Deliver<C> for ProxyDelivery<F> {
    fn deliver(&self, command: C) -> Result<(), C> {
        (self.0)(command)
    }
}

/// Direct owner-thread mailbox with an outstanding-work budget.
pub struct LocalDelivery<C> {
    transport: Arc<dyn Deliver<C>>,
    lifetime: ExecutorLifetime,
}

impl<C> Clone for LocalDelivery<C> {
    fn clone(&self) -> Self {
        Self {
            transport: self.transport.clone(),
            lifetime: self.lifetime.clone(),
        }
    }
}

impl<C> fmt::Debug for LocalDelivery<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LocalDelivery")
    }
}

impl<C: Send + 'static> LocalDelivery<C> {
    pub fn new(send: impl Fn(C) -> Result<(), C> + Send + Sync + 'static) -> Self {
        Self {
            transport: Arc::new(ProxyDelivery(send)),
            lifetime: ExecutorLifetime::default(),
        }
    }

    pub fn with_lifetime(mut self, lifetime: ExecutorLifetime) -> Self {
        self.lifetime = lifetime;
        self
    }
}

impl<C: Send + 'static> Default for LocalDelivery<C> {
    fn default() -> Self {
        Self::new(Err)
    }
}

impl<C: Send + 'static> From<std::sync::mpsc::Sender<C>> for LocalDelivery<C> {
    fn from(sender: std::sync::mpsc::Sender<C>) -> Self {
        Self {
            transport: Arc::new(sender),
            lifetime: ExecutorLifetime::default(),
        }
    }
}

impl<C: Send + 'static> From<tokio::sync::mpsc::UnboundedSender<C>> for LocalDelivery<C> {
    fn from(sender: tokio::sync::mpsc::UnboundedSender<C>) -> Self {
        Self {
            transport: Arc::new(sender),
            lifetime: ExecutorLifetime::default(),
        }
    }
}

/// Capacity follows work until completion, even when its receiver is dropped.
pub struct Completion<R> {
    sender: Option<oneshot::Sender<R>>,
    _permit: Option<OwnedSemaphorePermit>,
}

impl<R> Completion<R> {
    pub fn control() -> Self {
        Self {
            sender: None,
            _permit: None,
        }
    }
    pub fn send(self, value: R) {
        let Self { sender, _permit } = self;
        drop(_permit);
        if let Some(sender) = sender {
            let _ = sender.send(value);
        }
    }
}

impl<C: Send + 'static> LocalDelivery<C> {
    pub fn notify<R>(
        &self,
        command: impl FnOnce(Completion<R>) -> C,
        control: bool,
    ) -> Result<(), ReplyClosed> {
        let permit = if control {
            None
        } else {
            Some(
                self.lifetime
                    .admission()
                    .0
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| ReplyClosed)?,
            )
        };
        self.deliver(command(Completion {
            sender: None,
            _permit: permit,
        }))
    }

    pub fn submit<T, E>(
        &self,
        command: impl FnOnce(Completion<Result<T, E>>) -> C,
    ) -> Result<PendingReply<T, E>, ReplyClosed> {
        let permit = self
            .lifetime
            .admission()
            .0
            .clone()
            .try_acquire_owned()
            .map_err(|_| ReplyClosed)?;
        let (sender, receiver) = oneshot::channel();
        self.deliver(command(Completion {
            sender: Some(sender),
            _permit: Some(permit),
        }))?;
        Ok(receiver)
    }

    fn deliver(&self, command: C) -> Result<(), ReplyClosed> {
        self.transport.deliver(command).map_err(|_| {
            self.lifetime.begin_close();
            ReplyClosed
        })
    }
}
