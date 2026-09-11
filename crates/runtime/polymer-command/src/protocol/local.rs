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
                sender: $crate::LocalDelivery<[<$client Command>]>,
            }

            impl $client {
                pub(crate) fn new(
                    sender: impl Into<$crate::LocalDelivery<[<$client Command>]>>,
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
                        $($response_field: $crate::Completion<Result<$response, $error>>,)?
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
                        $($response_field: $crate::Completion<Result<$response, $error>>,)?
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
        [$([$argument_mode:ident] [$argument:ident] [$argument_type:ty])*] [$response:ty, $error:ty]
    ) => {
        $method_visibility fn $method(
            &self,
            $($argument: $crate::__command_argument_type!([$argument_mode] [$argument_type])),*
        ) {
            $($crate::__command_argument_convert!([$argument_mode] [$argument] [$argument_type]);)*
            if self.sender.notify(|completion| $command::$variant { $($argument,)* completion }, $crate::__command_is_control!($variant)).is_err() {
                $crate::__tracing::warn!(command = stringify!($variant), "command notification was not admitted");
            }
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
            $crate::PendingReply<$response, $error>,
            $error,
        > {
            $($crate::__command_argument_convert!([$argument_mode] [$argument] [$argument_type]);)*
            self.sender
                .submit(|response| $command::$variant { $($argument,)* response })
                .map_err(|_| $closed)
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
                let receiver = sender
                    .submit(|response| $command::$variant { $($argument,)* response })
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
            let receiver = self.sender
                .submit(|response| $command::$variant { $($argument,)* response })
                .map_err(|_| $closed)?;
            $crate::__block_on(receiver).map_err(|_| $closed)?
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __command_is_control {
    (Stop) => {
        true
    };
    (Close) => {
        true
    };
    ($other:ident) => {
        false
    };
}
