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
