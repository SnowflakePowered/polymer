//! Typed request/reply clients backed by command enums.

#![forbid(unsafe_code)]

use std::{future::Future, pin::Pin, sync::Arc};

pub use snowglass_command_macros::command_handler;

#[doc(hidden)]
pub use paste::paste as __paste;
#[doc(hidden)]
pub use tokio as __tokio;

pub type CommandFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandUnavailable;

/// Type-erased response sender used by generated command handlers.
pub enum CommandResponse<T> {
    Async(tokio::sync::oneshot::Sender<T>),
    Blocking(std::sync::mpsc::SyncSender<T>),
}

impl<T> CommandResponse<T> {
    pub fn send(self, response: T) -> Result<(), T> {
        match self {
            Self::Async(sender) => sender.send(response),
            Self::Blocking(sender) => sender.send(response).map_err(|error| error.0),
        }
    }
}

impl<T> From<tokio::sync::oneshot::Sender<T>> for CommandResponse<T> {
    fn from(sender: tokio::sync::oneshot::Sender<T>) -> Self {
        Self::Async(sender)
    }
}

impl<T> From<std::sync::mpsc::SyncSender<T>> for CommandResponse<T> {
    fn from(sender: std::sync::mpsc::SyncSender<T>) -> Self {
        Self::Blocking(sender)
    }
}

pub struct CommandSink<C>(Arc<dyn Fn(C) -> Result<(), CommandUnavailable> + Send + Sync>);

impl<C> Clone for CommandSink<C> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<C> std::fmt::Debug for CommandSink<C> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CommandSink")
    }
}

impl<C> CommandSink<C> {
    pub fn new(send: impl Fn(C) + Send + Sync + 'static) -> Self {
        Self::try_new(move |command| {
            send(command);
            Ok(())
        })
    }

    pub fn try_new(
        send: impl Fn(C) -> Result<(), CommandUnavailable> + Send + Sync + 'static,
    ) -> Self {
        Self(Arc::new(send))
    }

    pub fn send(&self, command: C) -> Result<(), CommandUnavailable> {
        (self.0)(command)
    }

    pub fn is_unique(&self) -> bool {
        Arc::strong_count(&self.0) == 1
    }
}

impl<C> Default for CommandSink<C> {
    fn default() -> Self {
        Self::new(|_| {})
    }
}

impl<C: Send + 'static> From<std::sync::mpsc::Sender<C>> for CommandSink<C> {
    fn from(sender: std::sync::mpsc::Sender<C>) -> Self {
        Self::try_new(move |command| sender.send(command).map_err(|_| CommandUnavailable))
    }
}

impl<C: Send + 'static> From<tokio::sync::mpsc::UnboundedSender<C>> for CommandSink<C> {
    fn from(sender: tokio::sync::mpsc::UnboundedSender<C>) -> Self {
        Self::try_new(move |command| sender.send(command).map_err(|_| CommandUnavailable))
    }
}

/// Declares a typed command enum, its sending client, and its handler trait.
///
/// Request/reply is the default. `async fn` waits asynchronously, a plain
/// `fn` blocks, `#[deferred]` returns the response receiver, and `#[oneshot]`
/// sends without creating a response channel. The generated command enum has
/// a `dispatch` method that exhaustively routes each variant to the generated
/// `<Client>CommandHandler` trait. Its generic associated output type permits
/// both immediate results and futures borrowing the handler.
#[macro_export]
macro_rules! command_client {
    (
        $visibility:vis client $client:ident {
            closed: $closed:expr;
            $($methods:tt)*
        }
    ) => {
        $crate::__command_client_parse! {
            @parse [$visibility] [$client] [$closed] []
            $($methods)*
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __command_client_parse {
    (
        @parse
        [$visibility:vis] [$client:ident] [$closed:expr]
        [$($commands:tt)*]
    ) => {
        $crate::__command_client_generate! {
            [$visibility] [$client] [$closed]
            $($commands)*
        }
    };

    (
        @parse
        [$visibility:vis] [$client:ident] [$closed:expr]
        [$($commands:tt)*]
        #[oneshot]
        #[args($variant:ident)]
        $method_visibility:vis fn $method:ident(
            $($argument:ident : $argument_type:ty),* $(,)?
        );
        $($remaining:tt)*
    ) => {
        $crate::__command_client_parse! {
            @parse [$visibility] [$client] [$closed]
            [
                $($commands)*
                [oneshot] [sync] [$variant] [$method_visibility] [$method]
                [$($argument : $argument_type),*] []
            ]
            $($remaining)*
        }
    };

    (
        @parse
        [$visibility:vis] [$client:ident] [$closed:expr]
        [$($commands:tt)*]
        #[deferred]
        #[args($variant:ident)]
        $method_visibility:vis fn $method:ident(
            $($argument:ident : $argument_type:ty),* $(,)?
        ) -> Result<$response:ty, $error:ty>;
        $($remaining:tt)*
    ) => {
        $crate::__command_client_parse! {
            @parse [$visibility] [$client] [$closed]
            [
                $($commands)*
                [deferred] [sync] [$variant] [$method_visibility] [$method]
                [$($argument : $argument_type),*] [response: $response, $error]
            ]
            $($remaining)*
        }
    };

    (
        @parse
        [$visibility:vis] [$client:ident] [$closed:expr]
        [$($commands:tt)*]
        #[args($variant:ident)]
        $method_visibility:vis async fn $method:ident(
            $($argument:ident : $argument_type:ty),* $(,)?
        ) -> Result<$response:ty, $error:ty>;
        $($remaining:tt)*
    ) => {
        $crate::__command_client_parse! {
            @parse [$visibility] [$client] [$closed]
            [
                $($commands)*
                [request] [asynchronous] [$variant] [$method_visibility] [$method]
                [$($argument : $argument_type),*] [response: $response, $error]
            ]
            $($remaining)*
        }
    };

    (
        @parse
        [$visibility:vis] [$client:ident] [$closed:expr]
        [$($commands:tt)*]
        #[args($variant:ident)]
        $method_visibility:vis fn $method:ident(
            $($argument:ident : $argument_type:ty),* $(,)?
        ) -> Result<$response:ty, $error:ty>;
        $($remaining:tt)*
    ) => {
        $crate::__command_client_parse! {
            @parse [$visibility] [$client] [$closed]
            [
                $($commands)*
                [request] [blocking] [$variant] [$method_visibility] [$method]
                [$($argument : $argument_type),*] [response: $response, $error]
            ]
            $($remaining)*
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __command_client_generate {
    (
        [$visibility:vis] [$client:ident] [$closed:expr]
        $(
            [$mode:ident] [$waiting:ident] [$variant:ident]
            [$method_visibility:vis] [$method:ident]
            [$($argument:ident : $argument_type:ty),*]
            [$($response_field:ident : $response:ty, $error:ty)?]
        )*
    ) => {
        $crate::__paste! {
            #[doc = concat!("Commands accepted by [`", stringify!($client), "`].")]
            $visibility enum [<$client Command>] {
                $(
                    $variant {
                        $($argument: $argument_type,)*
                        $($response_field: $crate::CommandResponse<Result<$response, $error>>,)?
                    },
                )*
            }

            impl std::fmt::Debug for [<$client Command>] {
                fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    match self {
                        $(Self::$variant { .. } => formatter.write_str(stringify!($variant)),)*
                    }
                }
            }

            #[doc = concat!("Handles commands sent through [`", stringify!($client), "`].")]
            $visibility trait [<$client CommandHandler>] {
                type Output<'handler>
                where
                    Self: 'handler;

                $(
                    fn $method<'handler>(
                        &'handler mut self,
                        $($argument: $argument_type,)*
                        $($response_field: $crate::CommandResponse<Result<$response, $error>>,)?
                    ) -> Self::Output<'handler>;
                )*
            }

            impl [<$client Command>] {
                pub fn dispatch<'handler, Handler>(
                    self,
                    handler: &'handler mut Handler,
                ) -> Handler::Output<'handler>
                where
                    Handler: [<$client CommandHandler>] + ?Sized,
                {
                    match self {
                        $(
                            Self::$variant {
                                $($argument,)*
                                $($response_field,)?
                            } => handler.$method($($argument,)* $($response_field,)?),
                        )*
                    }
                }
            }

            #[derive(Clone, Debug)]
            #[doc = concat!("Typed client for [`", stringify!([<$client Command>]), "`].")]
            $visibility struct $client {
                sender: $crate::CommandSink<[<$client Command>]>,
            }

            impl $client {
                pub(crate) fn new(
                    sender: impl Into<$crate::CommandSink<[<$client Command>]>>,
                ) -> Self {
                    Self { sender: sender.into() }
                }

                $(
                    $crate::__command_client_method! {
                        [$mode] [$waiting] [[<$client Command>]] [$closed]
                        [$method_visibility] [$method] [$variant]
                        [$($argument : $argument_type),*]
                        [$($response, $error)?]
                    }
                )*
            }
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __command_client_method {
    (
        [oneshot] [sync] [$command:ident] [$closed:expr]
        [$method_visibility:vis] [$method:ident] [$variant:ident]
        [$($argument:ident : $argument_type:ty),*] []
    ) => {
        $method_visibility fn $method(&self, $($argument: $argument_type),*) {
            let _ = self.sender.send($command::$variant { $($argument,)* });
        }
    };

    (
        [deferred] [sync] [$command:ident] [$closed:expr]
        [$method_visibility:vis] [$method:ident] [$variant:ident]
        [$($argument:ident : $argument_type:ty),*] [$response:ty, $error:ty]
    ) => {
        $method_visibility fn $method(
            &self,
            $($argument: $argument_type),*
        ) -> Result<
            $crate::__tokio::sync::oneshot::Receiver<Result<$response, $error>>,
            $error,
        > {
            let (response, receiver) = $crate::__tokio::sync::oneshot::channel();
            self.sender
                .send($command::$variant {
                    $($argument,)*
                    response: response.into(),
                })
                .map_err(|_| $closed)?;
            Ok(receiver)
        }
    };

    (
        [request] [asynchronous] [$command:ident] [$closed:expr]
        [$method_visibility:vis] [$method:ident] [$variant:ident]
        [$($argument:ident : $argument_type:ty),*] [$response:ty, $error:ty]
    ) => {
        $method_visibility fn $method(
            &self,
            $($argument: $argument_type),*
        ) -> $crate::CommandFuture<Result<$response, $error>> {
            let sender = self.sender.clone();
            Box::pin(async move {
                let (response, receiver) = $crate::__tokio::sync::oneshot::channel();
                sender
                    .send($command::$variant {
                        $($argument,)*
                        response: response.into(),
                    })
                    .map_err(|_| $closed)?;
                receiver.await.map_err(|_| $closed)?
            })
        }
    };

    (
        [request] [blocking] [$command:ident] [$closed:expr]
        [$method_visibility:vis] [$method:ident] [$variant:ident]
        [$($argument:ident : $argument_type:ty),*] [$response:ty, $error:ty]
    ) => {
        $method_visibility fn $method(
            &self,
            $($argument: $argument_type),*
        ) -> Result<$response, $error> {
            let (response, receiver) = std::sync::mpsc::sync_channel(1);
            self.sender
                .send($command::$variant {
                    $($argument,)*
                    response: response.into(),
                })
                .map_err(|_| $closed)?;
            receiver.recv().map_err(|_| $closed)?
        }
    };
}
