/// Declares a protocol implemented by an owned Kameo actor.
///
/// The generated wrapper lives with the protocol, allowing implementations in
/// downstream crates without coupling clients to the backend's concrete type.
#[macro_export]
macro_rules! native_client {
    ($visibility:vis client $client:ident {
        closed: $closed:expr;
        $($methods:tt)*
    }) => {
        $crate::__command_methods_parse! {
            @parse [native [$closed]] [$visibility] [$client] [] $($methods)*
        }
    };
}

#[macro_export]
macro_rules! actor_client {
    ($visibility:vis client $client:ident { $($body:tt)* }) => {
        $crate::actor_client! { $visibility client $client() { $($body)* } }
    };
    ($visibility:vis client $client:ident ($($field_visibility:vis $field:ident: $field_type:ty),* $(,)?) {
        closed: $closed:expr;
        queue_timeout: $attempts:expr, $queue_timeout:expr => $queue_error:expr;
        response_timeout: $reply_timeout:expr => $reply_error:expr;
        $($methods:tt)*
    }) => {
        $crate::__command_methods_parse! {
            @parse [native_actor [$closed] [$attempts] [$queue_timeout] [$queue_error]
                [$reply_timeout] [$reply_error] [$([$field_visibility] [$field] [$field_type])*]]
            [$visibility] [$client] [] $($methods)*
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __native_actor_client_generate {
    (
        [$closed:expr] [$attempts:expr] [$queue_timeout:expr] [$queue_error:expr]
        [$reply_timeout:expr] [$reply_error:expr]
        [$([$field_visibility:vis] [$field:ident] [$field_type:ty])*]
        [$visibility:vis] [$client:ident]
        $(
            [request] [asynchronous] [$variant:ident]
            [$method_visibility:vis] [$method:ident]
            [$([$argument_mode:ident] [$argument:ident] [$argument_type:ty])*]
            [$response_field:ident : $response:ty, $error:ty]
        )*
    ) => {
        $crate::__paste! {
            $(
                $visibility struct [<$client $variant>] { $(pub $argument: $argument_type,)* }
            )*

            #[derive(Clone, Debug)]
            $visibility struct $client {
                $($method: $crate::__kameo::actor::ReplyRecipient<$crate::Queued<[<$client $variant>]>, $response, $error>,)*
                policy: $crate::Admission,
                $($field_visibility $field: $field_type,)*
            }

            impl $client {
                pub(crate) fn from_actor<A>(
                    actor: $crate::__kameo::actor::ActorRef<A>,
                    policy: $crate::Admission,
                    $($field: $field_type,)*
                ) -> Self
                where
                    A: $crate::__kameo::Actor $(+ $crate::__kameo::message::Message<$crate::Queued<[<$client $variant>]>>)*,
                    $(<A as $crate::__kameo::message::Message<$crate::Queued<[<$client $variant>]>>>::Reply:
                        $crate::__kameo::reply::Reply<Ok = $response, Error = $error>,)*
                {
                    Self { $($method: actor.clone().reply_recipient(),)* policy, $($field,)* }
                }

                $(
                    $method_visibility fn $method(
                        &self,
                        $($argument: $crate::__command_argument_type!([$argument_mode] [$argument_type])),*
                    ) -> $crate::CommandFuture<Result<$response, $error>> {
                        $($crate::__command_argument_convert!([$argument_mode] [$argument] [$argument_type]);)*
                        let recipient = self.$method.clone();
                        let policy = self.policy.clone();
                        Box::pin(async move {
                            let message = $crate::__native_admit!($variant, policy,
                                [<$client $variant>] { $($argument,)* }, $attempts, $queue_timeout)
                                .map_err(|error| match error {
                                    $crate::AdmissionFailure::Closed => $closed,
                                    $crate::AdmissionFailure::Timeout => $queue_error,
                                })?;
                            $crate::__tokio::time::timeout($reply_timeout, recipient.ask(message).try_send()).await
                                .map_err(|_| $reply_error)?
                                .map_err(|error| match error {
                                    $crate::__kameo::error::SendError::HandlerError(error) => error,
                                    _ => $closed,
                                })

                        })
                    }
                )*
            }
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __native_admit {
    (Shutdown, $policy:ident, $message:expr, $attempts:expr, $timeout:expr) => {{
        let _ = &$policy;
        Ok::<_, $crate::AdmissionFailure>($crate::Queued {
            message: $message,
            permit: None,
        })
    }};
    ($variant:ident, $policy:ident, $message:expr, $attempts:expr, $timeout:expr) => {
        $policy.admit($message, $attempts, $timeout).await
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __native_client_generate {
    (
        [$closed:expr] [$visibility:vis] [$client:ident]
        $(
            [request] [asynchronous] [$variant:ident]
            [$method_visibility:vis] [$method:ident]
            [$([$argument_mode:ident] [$argument:ident] [$argument_type:ty])*]
            [$response_field:ident : $response:ty, $error:ty]
        )*
    ) => {
        $crate::__paste! {
            $visibility trait [<$client CommandHandler>]: Send + 'static {
                $(fn $method(&mut self, $($argument: $argument_type),*)
                    -> impl ::std::future::Future<Output = Result<$response, $error>> + Send;)*
            }

            $visibility struct [<$client Actor>]<H: [<$client CommandHandler>]>(H);

            impl<H: [<$client CommandHandler>]> $crate::__kameo::Actor for [<$client Actor>]<H> {
                type Args = H;
                type Error = ::std::convert::Infallible;

                async fn on_start(
                    state: H,
                    _: $crate::__kameo::actor::ActorRef<Self>,
                ) -> Result<Self, Self::Error> {
                    Ok(Self(state))
                }
            }

            $(
                $visibility struct [<$client $variant>] {
                    $(pub $argument: $argument_type,)*
                }

                impl<H: [<$client CommandHandler>]>
                    $crate::__kameo::message::Message<[<$client $variant>]> for [<$client Actor>]<H>
                {
                    type Reply = Result<$response, $error>;

                    async fn handle(
                        &mut self,
                        message: [<$client $variant>],
                        _: &mut $crate::__kameo::message::Context<Self, Self::Reply>,
                    ) -> Self::Reply {
                        self.0.$method($(message.$argument),*).await
                    }
                }
            )*

            #[derive(Clone, Debug)]
            $visibility struct $client {
                $($method: $crate::__kameo::actor::ReplyRecipient<[<$client $variant>], $response, $error>,)*
            }

            impl $client {
                pub fn spawn<H: [<$client CommandHandler>]>(handler: H) -> Self {
                    use $crate::__kameo::actor::Spawn;
                    let actor = [<$client Actor>]::<H>::spawn(handler);
                    Self { $($method: actor.clone().reply_recipient(),)* }
                }

                $(
                    $method_visibility fn $method(
                        &self,
                        $($argument: $crate::__command_argument_type!([$argument_mode] [$argument_type])),*
                    ) -> $crate::CommandFuture<Result<$response, $error>> {
                        $($crate::__command_argument_convert!([$argument_mode] [$argument] [$argument_type]);)*
                        let recipient = self.$method.clone();
                        Box::pin(async move {
                            recipient.ask([<$client $variant>] { $($argument,)* }).send().await
                                .map_err(|error| match error {
                                    $crate::__kameo::error::SendError::HandlerError(error) => error,
                                    _ => $closed,
                                })
                        })
                    }
                )*
            }
        }
    };
}
