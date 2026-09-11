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
            [oneshot] [asynchronous] [$variant] [$method_visibility] [$method] [completion: (), std::convert::Infallible]
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
            [oneshot] [sync] [$variant] [$method_visibility] [$method] [completion: (), std::convert::Infallible]
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
