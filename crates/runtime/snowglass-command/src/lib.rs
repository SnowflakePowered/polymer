//! Typed request/reply clients backed by command enums.

#![forbid(unsafe_code)]

use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

pub use snowglass_command_macros::command_handler;

#[doc(hidden)]
pub use paste::paste as __paste;
#[doc(hidden)]
pub use tokio as __tokio;

pub type CommandFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandUnavailable;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActorCommandUnavailable {
    Closed,
    Timeout,
}

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

pub struct ActorCommandSink<C>(
    Arc<dyn Fn(C) -> CommandFuture<Result<(), ActorCommandUnavailable>> + Send + Sync>,
);

impl<C> Clone for ActorCommandSink<C> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<C> std::fmt::Debug for ActorCommandSink<C> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ActorCommandSink")
    }
}

impl<C> ActorCommandSink<C> {
    pub fn try_new<SendFn, SendFuture>(send: SendFn) -> Self
    where
        SendFn: Fn(C) -> SendFuture + Send + Sync + 'static,
        SendFuture: Future<Output = Result<(), ActorCommandUnavailable>> + Send + 'static,
    {
        Self(Arc::new(move |command| Box::pin(send(command))))
    }

    pub fn bounded(sender: tokio::sync::mpsc::Sender<C>, attempts: usize, timeout: Duration) -> Self
    where
        C: Send + 'static,
    {
        Self::bounded_map(sender, |command| command, attempts, timeout)
    }

    pub fn bounded_map<Message>(
        sender: tokio::sync::mpsc::Sender<Message>,
        map: impl Fn(C) -> Message + Send + Sync + 'static,
        attempts: usize,
        timeout: Duration,
    ) -> Self
    where
        C: Send + 'static,
        Message: Send + 'static,
    {
        let map = Arc::new(map);
        Self::try_new(move |command| {
            let sender = sender.clone();
            let map = map.clone();
            async move {
                for attempt in 1..=attempts.max(1) {
                    match tokio::time::timeout(timeout, sender.reserve()).await {
                        Ok(Ok(permit)) => {
                            permit.send(map(command));
                            return Ok(());
                        }
                        Ok(Err(_)) => return Err(ActorCommandUnavailable::Closed),
                        Err(_) if attempt < attempts.max(1) => tokio::task::yield_now().await,
                        Err(_) => return Err(ActorCommandUnavailable::Timeout),
                    }
                }
                unreachable!("actor command send attempts are nonzero")
            }
        })
    }

    pub fn send(&self, command: C) -> CommandFuture<Result<(), ActorCommandUnavailable>> {
        (self.0)(command)
    }

    pub fn is_unique(&self) -> bool {
        Arc::strong_count(&self.0) == 1
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
        $crate::__command_methods_parse! {
            @parse [immediate [$closed]] [$visibility] [$client] []
            $($methods)*
        }
    };
}

/// Declares a typed command client backed by a bounded asynchronous actor queue.
///
/// Queue admission is retried according to `queue_timeout`; request/reply methods
/// additionally wait up to `response_timeout` for the handler response.
#[macro_export]
macro_rules! actor_client {
    (
        $visibility:vis client $client:ident
        $(
            (
                $($field_visibility:vis $field:ident : $field_type:ty),* $(,)?
            )
        )?
        {
            closed: $closed:expr;
            queue_timeout:
                $queue_attempts:expr,
                $queue_timeout:expr
                => $queue_timeout_error:expr;
            response_timeout:
                $response_timeout:expr
                => $response_timeout_error:expr;
            $($methods:tt)*
        }
    ) => {
        $crate::__command_methods_parse! {
            @parse
            [actor
                [$closed]
                [$queue_attempts]
                [$queue_timeout]
                [$queue_timeout_error]
                [$response_timeout]
                [$response_timeout_error]
                [fields $($([$field_visibility] [$field] [$field_type])*)?]
            ]
            [$visibility] [$client] []
            $($methods)*
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __command_methods_parse {
    (
        @parse
        [$backend:ident $($backend_configuration:tt)*]
        [$visibility:vis] [$client:ident]
        [$($commands:tt)*]
    ) => {
        $crate::__command_client_generate! {
            [$backend $($backend_configuration)*]
            [$visibility] [$client]
            $($commands)*
        }
    };

    (
        @parse
        [$backend:ident $($backend_configuration:tt)*]
        [$visibility:vis] [$client:ident]
        [$($commands:tt)*]
        #[oneshot]
        #[args($variant:ident)]
        $method_visibility:vis async fn $method:ident($($arguments:tt)*);
        $($remaining:tt)*
    ) => {
        $crate::__command_arguments_parse! {
            @parse
            [$backend $($backend_configuration)*] [$visibility] [$client]
            [$($commands)*]
            [oneshot] [asynchronous] [$variant] [$method_visibility] [$method] []
            [$($remaining)*] []
            $($arguments)*
        }
    };

    (
        @parse
        [$backend:ident $($backend_configuration:tt)*]
        [$visibility:vis] [$client:ident]
        [$($commands:tt)*]
        #[oneshot]
        #[args($variant:ident)]
        $method_visibility:vis fn $method:ident($($arguments:tt)*);
        $($remaining:tt)*
    ) => {
        $crate::__command_arguments_parse! {
            @parse
            [$backend $($backend_configuration)*] [$visibility] [$client]
            [$($commands)*]
            [oneshot] [sync] [$variant] [$method_visibility] [$method] []
            [$($remaining)*] []
            $($arguments)*
        }
    };

    (
        @parse
        [$backend:ident $($backend_configuration:tt)*]
        [$visibility:vis] [$client:ident]
        [$($commands:tt)*]
        #[deferred]
        #[args($variant:ident)]
        $method_visibility:vis fn $method:ident($($arguments:tt)*)
            -> Result<$response:ty, $error:ty>;
        $($remaining:tt)*
    ) => {
        $crate::__command_arguments_parse! {
            @parse
            [$backend $($backend_configuration)*] [$visibility] [$client]
            [$($commands)*]
            [deferred] [sync] [$variant] [$method_visibility] [$method]
            [response: $response, $error]
            [$($remaining)*] []
            $($arguments)*
        }
    };

    (
        @parse
        [$backend:ident $($backend_configuration:tt)*]
        [$visibility:vis] [$client:ident]
        [$($commands:tt)*]
        #[args($variant:ident)]
        $method_visibility:vis async fn $method:ident($($arguments:tt)*)
            -> Result<$response:ty, $error:ty>;
        $($remaining:tt)*
    ) => {
        $crate::__command_arguments_parse! {
            @parse
            [$backend $($backend_configuration)*] [$visibility] [$client]
            [$($commands)*]
            [request] [asynchronous] [$variant] [$method_visibility] [$method]
            [response: $response, $error]
            [$($remaining)*] []
            $($arguments)*
        }
    };

    (
        @parse
        [$backend:ident $($backend_configuration:tt)*]
        [$visibility:vis] [$client:ident]
        [$($commands:tt)*]
        #[args($variant:ident)]
        $method_visibility:vis fn $method:ident($($arguments:tt)*)
            -> Result<$response:ty, $error:ty>;
        $($remaining:tt)*
    ) => {
        $crate::__command_arguments_parse! {
            @parse
            [$backend $($backend_configuration)*] [$visibility] [$client]
            [$($commands)*]
            [request] [blocking] [$variant] [$method_visibility] [$method]
            [response: $response, $error]
            [$($remaining)*] []
            $($arguments)*
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __command_arguments_parse {
    (
        @parse
        [$backend:ident $($backend_configuration:tt)*]
        [$visibility:vis] [$client:ident] [$($commands:tt)*]
        [$mode:ident] [$waiting:ident] [$variant:ident]
        [$method_visibility:vis] [$method:ident] [$($response:tt)*]
        [$($remaining_methods:tt)*]
        [$($arguments:tt)*]
    ) => {
        $crate::__command_methods_parse! {
            @parse [$backend $($backend_configuration)*] [$visibility] [$client]
            [
                $($commands)*
                [$mode] [$waiting] [$variant] [$method_visibility] [$method]
                [$($arguments)*] [$($response)*]
            ]
            $($remaining_methods)*
        }
    };

    (
        @parse $backend:tt $visibility:tt $client:tt $commands:tt
        $mode:tt $waiting:tt $variant:tt $method_visibility:tt $method:tt $response:tt
        $remaining_methods:tt [$($arguments:tt)*]
        $argument:ident : impl Into<$argument_type:ty>, $($remaining:tt)*
    ) => {
        $crate::__command_arguments_parse! {
            @parse $backend $visibility $client $commands
            $mode $waiting $variant $method_visibility $method $response
            $remaining_methods
            [$($arguments)* [into] [$argument] [$argument_type]]
            $($remaining)*
        }
    };

    (
        @parse $backend:tt $visibility:tt $client:tt $commands:tt
        $mode:tt $waiting:tt $variant:tt $method_visibility:tt $method:tt $response:tt
        $remaining_methods:tt [$($arguments:tt)*]
        $argument:ident : impl Into<$argument_type:ty>
    ) => {
        $crate::__command_arguments_parse! {
            @parse $backend $visibility $client $commands
            $mode $waiting $variant $method_visibility $method $response
            $remaining_methods
            [$($arguments)* [into] [$argument] [$argument_type]]
        }
    };

    (
        @parse $backend:tt $visibility:tt $client:tt $commands:tt
        $mode:tt $waiting:tt $variant:tt $method_visibility:tt $method:tt $response:tt
        $remaining_methods:tt [$($arguments:tt)*]
        $argument:ident : $argument_type:ty, $($remaining:tt)*
    ) => {
        $crate::__command_arguments_parse! {
            @parse $backend $visibility $client $commands
            $mode $waiting $variant $method_visibility $method $response
            $remaining_methods
            [$($arguments)* [plain] [$argument] [$argument_type]]
            $($remaining)*
        }
    };

    (
        @parse $backend:tt $visibility:tt $client:tt $commands:tt
        $mode:tt $waiting:tt $variant:tt $method_visibility:tt $method:tt $response:tt
        $remaining_methods:tt [$($arguments:tt)*]
        $argument:ident : $argument_type:ty
    ) => {
        $crate::__command_arguments_parse! {
            @parse $backend $visibility $client $commands
            $mode $waiting $variant $method_visibility $method $response
            $remaining_methods
            [$($arguments)* [plain] [$argument] [$argument_type]]
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __command_client_generate {
    (
        [immediate [$closed:expr]]
        [$visibility:vis] [$client:ident]
        $(
            [$mode:ident] [$waiting:ident] [$variant:ident]
            [$method_visibility:vis] [$method:ident]
            [$([$argument_mode:ident] [$argument:ident] [$argument_type:ty])*]
            [$($response_field:ident : $response:ty, $error:ty)?]
        )*
    ) => {
        $crate::__command_protocol_generate! {
            [$visibility] [$client]
            $(
                [$mode] [$waiting] [$variant]
                [$method_visibility] [$method]
                [$([$argument_mode] [$argument] [$argument_type])*]
                [$($response_field : $response, $error)?]
            )*
        }
        $crate::__paste! {
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
                        [$([$argument_mode] [$argument] [$argument_type])*]
                        [$($response, $error)?]
                    }
                )*
            }
        }
    };

    (
        [actor
            [$closed:expr]
            [$queue_attempts:expr]
            [$queue_timeout:expr]
            [$queue_timeout_error:expr]
            [$response_timeout:expr]
            [$response_timeout_error:expr]
            [fields $([$field_visibility:vis] [$field:ident] [$field_type:ty])*]
        ]
        [$visibility:vis] [$client:ident]
        $(
            [$mode:ident] [$waiting:ident] [$variant:ident]
            [$method_visibility:vis] [$method:ident]
            [$([$argument_mode:ident] [$argument:ident] [$argument_type:ty])*]
            [$($response_field:ident : $response:ty, $error:ty)?]
        )*
    ) => {
        $crate::__command_protocol_generate! {
            [$visibility] [$client]
            $(
                [$mode] [$waiting] [$variant]
                [$method_visibility] [$method]
                [$([$argument_mode] [$argument] [$argument_type])*]
                [$($response_field : $response, $error)?]
            )*
        }
        $crate::__paste! {
            #[derive(Clone, Debug)]
            #[doc = concat!("Typed bounded actor client for [`", stringify!([<$client Command>]), "`].")]
            $visibility struct $client {
                sender: $crate::ActorCommandSink<[<$client Command>]>,
                $($field_visibility $field: $field_type,)*
            }

            impl $client {
                pub(crate) fn new(
                    sender: $crate::__tokio::sync::mpsc::Sender<[<$client Command>]>,
                    $($field: $field_type,)*
                ) -> Self {
                    Self {
                        sender: $crate::ActorCommandSink::bounded(
                            sender,
                            $queue_attempts,
                            $queue_timeout,
                        ),
                        $($field,)*
                    }
                }

                pub(crate) fn new_mapped<Message>(
                    sender: $crate::__tokio::sync::mpsc::Sender<Message>,
                    map: impl Fn([<$client Command>]) -> Message + Send + Sync + 'static,
                    $($field: $field_type,)*
                ) -> Self
                where
                    Message: Send + 'static,
                {
                    Self {
                        sender: $crate::ActorCommandSink::bounded_map(
                            sender,
                            map,
                            $queue_attempts,
                            $queue_timeout,
                        ),
                        $($field,)*
                    }
                }

                $(
                    $crate::__actor_client_method! {
                        [$mode] [$waiting] [[<$client Command>]]
                        [$closed] [$queue_timeout_error]
                        [$response_timeout] [$response_timeout_error]
                        [$method_visibility] [$method] [$variant]
                        [$([$argument_mode] [$argument] [$argument_type])*]
                        [$($response, $error)?]
                    }
                )*
            }
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __command_protocol_generate {
    (
        [$visibility:vis] [$client:ident]
        $(
            [$mode:ident] [$waiting:ident] [$variant:ident]
            [$method_visibility:vis] [$method:ident]
            [$([$argument_mode:ident] [$argument:ident] [$argument_type:ty])*]
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
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __command_client_method {
    (
        [oneshot] [sync] [$command:ident] [$closed:expr]
        [$method_visibility:vis] [$method:ident] [$variant:ident]
        [$([$argument_mode:ident] [$argument:ident] [$argument_type:ty])*] []
    ) => {
        $method_visibility fn $method(
            &self,
            $($argument: $crate::__command_argument_type!([$argument_mode] [$argument_type])),*
        ) {
            $($crate::__command_argument_convert!([$argument_mode] [$argument] [$argument_type]);)*
            let _ = self.sender.send($command::$variant { $($argument,)* });
        }
    };

    (
        [deferred] [sync] [$command:ident] [$closed:expr]
        [$method_visibility:vis] [$method:ident] [$variant:ident]
        [$([$argument_mode:ident] [$argument:ident] [$argument_type:ty])*]
        [$response:ty, $error:ty]
    ) => {
        $method_visibility fn $method(
            &self,
            $($argument: $crate::__command_argument_type!([$argument_mode] [$argument_type])),*
        ) -> Result<
            $crate::__tokio::sync::oneshot::Receiver<Result<$response, $error>>,
            $error,
        > {
            $($crate::__command_argument_convert!([$argument_mode] [$argument] [$argument_type]);)*
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
        [$([$argument_mode:ident] [$argument:ident] [$argument_type:ty])*]
        [$response:ty, $error:ty]
    ) => {
        $method_visibility fn $method(
            &self,
            $($argument: $crate::__command_argument_type!([$argument_mode] [$argument_type])),*
        ) -> $crate::CommandFuture<Result<$response, $error>> {
            $($crate::__command_argument_convert!([$argument_mode] [$argument] [$argument_type]);)*
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
        [$([$argument_mode:ident] [$argument:ident] [$argument_type:ty])*]
        [$response:ty, $error:ty]
    ) => {
        $method_visibility fn $method(
            &self,
            $($argument: $crate::__command_argument_type!([$argument_mode] [$argument_type])),*
        ) -> Result<$response, $error> {
            $($crate::__command_argument_convert!([$argument_mode] [$argument] [$argument_type]);)*
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

#[doc(hidden)]
#[macro_export]
macro_rules! __actor_client_method {
    (
        [request] [asynchronous] [$command:ident]
        [$closed:expr] [$queue_timeout_error:expr]
        [$response_timeout:expr] [$response_timeout_error:expr]
        [$method_visibility:vis] [$method:ident] [$variant:ident]
        [$([$argument_mode:ident] [$argument:ident] [$argument_type:ty])*]
        [$response:ty, $error:ty]
    ) => {
        $method_visibility fn $method(
            &self,
            $($argument: $crate::__command_argument_type!([$argument_mode] [$argument_type])),*
        ) -> $crate::CommandFuture<Result<$response, $error>> {
            $($crate::__command_argument_convert!([$argument_mode] [$argument] [$argument_type]);)*
            let sender = self.sender.clone();
            Box::pin(async move {
                let (response, receiver) = $crate::__tokio::sync::oneshot::channel();
                match sender
                    .send($command::$variant {
                        $($argument,)*
                        response: response.into(),
                    })
                    .await
                {
                    Ok(()) => {}
                    Err($crate::ActorCommandUnavailable::Closed) => return Err($closed),
                    Err($crate::ActorCommandUnavailable::Timeout) => {
                        return Err($queue_timeout_error);
                    }
                }
                $crate::__tokio::time::timeout($response_timeout, receiver)
                    .await
                    .map_err(|_| $response_timeout_error)?
                    .map_err(|_| $closed)?
            })
        }
    };

    (
        [oneshot] [asynchronous] [$command:ident]
        [$closed:expr] [$queue_timeout_error:expr]
        [$response_timeout:expr] [$response_timeout_error:expr]
        [$method_visibility:vis] [$method:ident] [$variant:ident]
        [$([$argument_mode:ident] [$argument:ident] [$argument_type:ty])*] []
    ) => {
        $method_visibility fn $method(
            &self,
            $($argument: $crate::__command_argument_type!([$argument_mode] [$argument_type])),*
        ) -> $crate::CommandFuture<()> {
            $($crate::__command_argument_convert!([$argument_mode] [$argument] [$argument_type]);)*
            let sender = self.sender.clone();
            Box::pin(async move {
                let _ = sender
                    .send($command::$variant { $($argument,)* })
                    .await;
            })
        }
    };

    (
        [$mode:ident] [$waiting:ident] [$command:ident]
        [$closed:expr] [$queue_timeout_error:expr]
        [$response_timeout:expr] [$response_timeout_error:expr]
        [$method_visibility:vis] [$method:ident] [$variant:ident]
        [$([$argument_mode:ident] [$argument:ident] [$argument_type:ty])*]
        [$($response:ty, $error:ty)?]
    ) => {
        compile_error!("actor_client! methods must be async");
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __command_argument_type {
    ([plain] [$argument_type:ty]) => { $argument_type };
    ([into] [$argument_type:ty]) => { impl Into<$argument_type> };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __command_argument_convert {
    ([plain] [$argument:ident] [$argument_type:ty]) => {};
    ([into] [$argument:ident] [$argument_type:ty]) => {
        let $argument: $argument_type = $argument.into();
    };
}
