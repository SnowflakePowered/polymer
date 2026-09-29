#[doc(hidden)]
#[macro_export]
macro_rules! __chain_hooks_call {
    (@infix_slice) => { &[] };
    (@infix_slice infix($($hook:path),* $(,)?)) => { &[$($hook),*] };
    (@infix_slice_as $hook_type:ty;) => { &[] as &[$hook_type] };
    (@infix_slice_as $hook_type:ty; infix($($hook:path),* $(,)?)) => {
        &[$($hook as $hook_type),*]
    };

    (@prefix_slice [$($abi:tt)*] [$context:ident] [$callback_context:ident] [$($adapter:expr,)*];) => {
        &[$($adapter),*]
    };
    (@prefix_slice
        [$(($argument:ident, $argument_type:ty))*]
        [$context:ident]
        [$callback_context:ident]
        [$($adapter:expr,)*];
        prefix($($hooks:tt)*)
    ) => {
        $crate::__chain_hooks_call!(@prefix_adapters
            [$(($argument, $argument_type))*] [$context] [$callback_context] [$($adapter,)*]; $($hooks)*
        )
    };
    (@prefix_adapters
        [$(($argument:ident, $argument_type:ty))*]
        [$context:ident]
        [$callback_context:ident]
        [$($adapter:expr,)*];
    ) => { &[$($adapter),*] };
    (@prefix_adapters
        [$(($argument:ident, $argument_type:ty))*]
        [$context:ident]
        [yes]
        [$($adapter:expr,)*];
        |$projection_context:ident| $observer:expr $(, $($remaining:tt)*)?
    ) => {
        $crate::__chain_hooks_call!(@prefix_adapters
            [$(($argument, $argument_type))*]
            [$context]
            [yes]
            [$($adapter,)*
                |$($argument: $argument_type,)* context: &$context<'_>| {
                    let _ = ($(&$argument,)*);
                    let $projection_context = context;
                    $observer
                },
            ];
            $($($remaining)*)?
        )
    };

    (@prefix_adapters
        [$(($argument:ident, $argument_type:ty))*]
        [$context:ident]
        [$callback_context:ident]
        [$($adapter:expr,)*];
        $($hook:ident)::+($($hook_argument:ident),*) $(, $($remaining:tt)*)?
    ) => {
        $crate::__chain_hooks_call!(@prefix_adapters
            [$(($argument, $argument_type))*]
            [$context]
            [$callback_context]
            [$($adapter,)*
                |$($argument: $argument_type,)* context: &$context<'_>| {
                    let _ = ($(&$argument,)*);
                    $crate::__chain_hooks_call!(@call_prefix [$callback_context] context;
                        [$($hook)::+]; $($hook_argument),*
                    )
                },
            ];
            $($($remaining)*)?
        )
    };

    (@call_prefix [no] $context:expr; [$($hook:ident)::+]; $($argument:ident),*) => {{
        let _ = $context;
        unsafe { $($hook)::+ ($($argument),*) }
    }};
    (@call_prefix [yes] $context:expr; [$($hook:ident)::+]; $($argument:ident),*) => {
        unsafe { $($hook)::+ ($($argument,)* $context) }
    };

    (@postfix_slice [$($abi:tt)*] [$result_type:ty] [$context:ident] [$callback_context:ident] [$($adapter:expr,)*];) => {
        &[$($adapter),*]
    };
    (@postfix_slice
        [$(($argument:ident, $argument_type:ty))*]
        [$result_type:ty]
        [$context:ident]
        [$callback_context:ident]
        [$($adapter:expr,)*];
        postfix($($hooks:tt)*)
    ) => {
        $crate::__chain_hooks_call!(@postfix_adapters
            [$(($argument, $argument_type))*] [$result_type] [$context] [$callback_context] [$($adapter,)*]; $($hooks)*
        )
    };
    (@postfix_adapters
        [$(($argument:ident, $argument_type:ty))*]
        [$result_type:ty]
        [$context:ident]
        [$callback_context:ident]
        [$($adapter:expr,)*];
    ) => { &[$($adapter),*] };
    (@postfix_adapters
        [$(($argument:ident, $argument_type:ty))*]
        [$result_type:ty]
        [$context:ident]
        [yes]
        [$($adapter:expr,)*];
        |$projection_result:ident, $projection_context:ident| $observer:expr
        $(, $($remaining:tt)*)?
    ) => {
        $crate::__chain_hooks_call!(@postfix_adapters
            [$(($argument, $argument_type))*]
            [$result_type]
            [$context]
            [yes]
            [$($adapter,)*
                |$($argument: $argument_type,)* result: $result_type, context: &$context<'_>| {
                    let _ = ($(&$argument,)*);
                    let $projection_result = result;
                    let $projection_context = context;
                    $observer
                },
            ];
            $($($remaining)*)?
        )
    };

    (@postfix_adapters
        [$(($argument:ident, $argument_type:ty))*]
        [$result_type:ty]
        [$context:ident]
        [$callback_context:ident]
        [$($adapter:expr,)*];
        $($hook:ident)::+($($hook_argument:ident),*) $(, $($remaining:tt)*)?
    ) => {
        $crate::__chain_hooks_call!(@postfix_adapters
            [$(($argument, $argument_type))*]
            [$result_type]
            [$context]
            [$callback_context]
            [$($adapter,)*
                |$($argument: $argument_type,)* result: $result_type, context: &$context<'_>| {
                    let _ = ($(&$argument,)*);
                    $crate::__chain_hooks_call!(@call_post_mode [$callback_context]
                        result; context; [$($hook)::+];
                        $($hook_argument),*
                    )
                },
            ];
            $($($remaining)*)?
        )
    };

    (@call_post_mode [no] $result:ident; $context:expr; [$($hook:ident)::+];
        $($argument:ident),*
    ) => {{
        let _ = $context;
        $crate::__chain_hooks_call!(@call_post $result; []; [$($hook)::+]; []; $($argument),*)
    }};
    (@call_post_mode [yes] $result:ident; $context:expr; [$($hook:ident)::+];
        $($argument:ident),*
    ) => {
        $crate::__chain_hooks_call!(@call_post $result; [$context,]; [$($hook)::+]; [];
            $($argument),*
        )
    };

    (@is_reverse forward) => { false };
    (@is_reverse reverse) => { true };

    (@call_post $result:ident; [$($suffix:expr,)*]; [$($hook:ident)::+]; [$($mapped:expr,)*];) => {
        unsafe { $($hook)::+ ($($mapped,)* $($suffix),*) }
    };
    (@call_post $result:ident; [$($suffix:expr,)*]; [$($hook:ident)::+]; [$($mapped:expr,)*];
        result $(, $($remaining:ident),*)?
    ) => {
        $crate::__chain_hooks_call!(@call_post $result; [$($suffix,)*]; [$($hook)::+];
            [$($mapped,)* $result,]; $($($remaining),*)?
        )
    };
    (@call_post $result:ident; [$($suffix:expr,)*]; [$($hook:ident)::+]; [$($mapped:expr,)*];
        $argument:ident $(, $($remaining:ident),*)?
    ) => {
        $crate::__chain_hooks_call!(@call_post $result; [$($suffix,)*]; [$($hook)::+];
            [$($mapped,)* $argument,]; $($($remaining),*)?
        )
    };
}
