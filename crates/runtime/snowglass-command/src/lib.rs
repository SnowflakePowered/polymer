//! Typed request/reply clients backed by command enums.

#![forbid(unsafe_code)]

use std::{future::Future, pin::Pin, sync::Arc};

#[doc(hidden)]
pub use paste::paste as __paste;
#[doc(hidden)]
pub use tokio as __tokio;

pub type CommandFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandUnavailable;

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

/// Declares a typed command enum and the client that sends it.
///
/// Request/reply is the default. `async fn` waits asynchronously, a plain
/// `fn` blocks, `#[deferred]` returns the response receiver, and `#[oneshot]`
/// sends without creating a response channel.
#[macro_export]
macro_rules! command_client {
    (
        $visibility:vis client $client:ident {
            closed: $closed:expr;
            $($methods:tt)*
        }
    ) => {
        $crate::__command_client_parse! {
            @parse
            [$visibility]
            [$client]
            [$closed]
            []
            []
            []
            $($methods)*
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __command_client_parse {
    (
        @parse
        [$visibility:vis]
        [$client:ident]
        [$closed:expr]
        [$($variants:tt)*]
        [$($implementations:tt)*]
        [$($debug_variants:ident,)*]
    ) => {
        $crate::__paste! {
            #[doc = concat!("Commands accepted by [`", stringify!($client), "`].")]
            $visibility enum [<$client Command>] {
                $($variants)*
            }

            impl std::fmt::Debug for [<$client Command>] {
                fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    match self {
                        $(
                            Self::$debug_variants { .. } => {
                                formatter.write_str(stringify!($debug_variants))
                            }
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

                $($implementations)*
            }
        }
    };

    (
        @parse
        [$visibility:vis]
        [$client:ident]
        [$closed:expr]
        [$($variants:tt)*]
        [$($implementations:tt)*]
        [$($debug_variants:ident,)*]
        #[args($variant:ident)]
        #[oneshot]
        $($remaining:tt)*
    ) => {
        $crate::__command_client_parse! {
            @parse
            [$visibility]
            [$client]
            [$closed]
            [$($variants)*]
            [$($implementations)*]
            [$($debug_variants,)*]
            #[oneshot]
            #[args($variant)]
            $($remaining)*
        }
    };

    (
        @parse
        [$visibility:vis]
        [$client:ident]
        [$closed:expr]
        [$($variants:tt)*]
        [$($implementations:tt)*]
        [$($debug_variants:ident,)*]
        #[args($variant:ident)]
        #[deferred]
        $($remaining:tt)*
    ) => {
        $crate::__command_client_parse! {
            @parse
            [$visibility]
            [$client]
            [$closed]
            [$($variants)*]
            [$($implementations)*]
            [$($debug_variants,)*]
            #[deferred]
            #[args($variant)]
            $($remaining)*
        }
    };

    (
        @parse
        [$visibility:vis]
        [$client:ident]
        [$closed:expr]
        [$($variants:tt)*]
        [$($implementations:tt)*]
        [$($debug_variants:ident,)*]
        #[oneshot]
        #[args($variant:ident)]
        $method_visibility:vis fn $method:ident(
            $($argument:ident : $argument_type:ty),* $(,)?
        );
        $($remaining:tt)*
    ) => {
        $crate::__command_client_parse! {
            @parse
            [$visibility]
            [$client]
            [$closed]
            [
                $($variants)*
                $variant { $($argument: $argument_type,)* },
            ]
            [
                $($implementations)*
                $method_visibility fn $method(&self, $($argument: $argument_type),*) {
                    let _ = self.sender.send([<$client Command>]::$variant { $($argument,)* });
                }
            ]
            [
                $($debug_variants,)*
                $variant,
            ]
            $($remaining)*
        }
    };

    (
        @parse
        [$visibility:vis]
        [$client:ident]
        [$closed:expr]
        [$($variants:tt)*]
        [$($implementations:tt)*]
        [$($debug_variants:ident,)*]
        #[deferred]
        #[args($variant:ident)]
        $method_visibility:vis fn $method:ident(
            $($argument:ident : $argument_type:ty),* $(,)?
        ) -> Result<$response:ty, $error:ty>;
        $($remaining:tt)*
    ) => {
        $crate::__command_client_parse! {
            @parse
            [$visibility]
            [$client]
            [$closed]
            [
                $($variants)*
                $variant {
                    $($argument: $argument_type,)*
                    response: $crate::__tokio::sync::oneshot::Sender<Result<$response, $error>>,
                },
            ]
            [
                $($implementations)*
                $method_visibility fn $method(
                    &self,
                    $($argument: $argument_type),*
                ) -> Result<
                    $crate::__tokio::sync::oneshot::Receiver<Result<$response, $error>>,
                    $error,
                > {
                    let (response, receiver) = $crate::__tokio::sync::oneshot::channel();
                    self.sender
                        .send([<$client Command>]::$variant { $($argument,)* response })
                        .map_err(|_| $closed)?;
                    Ok(receiver)
                }
            ]
            [
                $($debug_variants,)*
                $variant,
            ]
            $($remaining)*
        }
    };

    (
        @parse
        [$visibility:vis]
        [$client:ident]
        [$closed:expr]
        [$($variants:tt)*]
        [$($implementations:tt)*]
        [$($debug_variants:ident,)*]
        #[args($variant:ident)]
        $method_visibility:vis async fn $method:ident(
            $($argument:ident : $argument_type:ty),* $(,)?
        ) -> Result<$response:ty, $error:ty>;
        $($remaining:tt)*
    ) => {
        $crate::__command_client_parse! {
            @parse
            [$visibility]
            [$client]
            [$closed]
            [
                $($variants)*
                $variant {
                    $($argument: $argument_type,)*
                    response: $crate::__tokio::sync::oneshot::Sender<Result<$response, $error>>,
                },
            ]
            [
                $($implementations)*
                $method_visibility fn $method(
                    &self,
                    $($argument: $argument_type),*
                ) -> $crate::CommandFuture<Result<$response, $error>> {
                    let sender = self.sender.clone();
                    Box::pin(async move {
                        let (response, receiver) = $crate::__tokio::sync::oneshot::channel();
                        sender
                            .send([<$client Command>]::$variant { $($argument,)* response })
                            .map_err(|_| $closed)?;
                        receiver.await.map_err(|_| $closed)?
                    })
                }
            ]
            [
                $($debug_variants,)*
                $variant,
            ]
            $($remaining)*
        }
    };

    (
        @parse
        [$visibility:vis]
        [$client:ident]
        [$closed:expr]
        [$($variants:tt)*]
        [$($implementations:tt)*]
        [$($debug_variants:ident,)*]
        #[args($variant:ident)]
        $method_visibility:vis fn $method:ident(
            $($argument:ident : $argument_type:ty),* $(,)?
        ) -> Result<$response:ty, $error:ty>;
        $($remaining:tt)*
    ) => {
        $crate::__command_client_parse! {
            @parse
            [$visibility]
            [$client]
            [$closed]
            [
                $($variants)*
                $variant {
                    $($argument: $argument_type,)*
                    response: std::sync::mpsc::SyncSender<Result<$response, $error>>,
                },
            ]
            [
                $($implementations)*
                $method_visibility fn $method(
                    &self,
                    $($argument: $argument_type),*
                ) -> Result<$response, $error> {
                    let (response, receiver) = std::sync::mpsc::sync_channel(1);
                    self.sender
                        .send([<$client Command>]::$variant { $($argument,)* response })
                        .map_err(|_| $closed)?;
                    receiver.recv().map_err(|_| $closed)?
                }
            ]
            [
                $($debug_variants,)*
                $variant,
            ]
            $($remaining)*
        }
    };
}
