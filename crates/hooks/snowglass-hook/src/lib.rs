//! Shared typed-hook infrastructure.

pub mod config;
mod vtable;

#[doc(hidden)]
pub use dashmap::DashMap as __DashMap;
#[doc(hidden)]
pub use paste::paste as __paste;
pub use vtable::VtableHook;

/// A continuation over a typed hook chain.
pub trait HookChain<'a, T: 'a> {
    /// Advances to the next registered hook, if one remains.
    fn fp_next(&mut self) -> Option<&'a T>;
}

/// Advances a terminal-backed hook chain and invokes its continuation.
///
/// The context is passed to the continuation after the listed ABI arguments.
/// This consumes the context, so clone or copy anything needed after the call
/// before invoking `next!`.
#[macro_export]
macro_rules! next {
    ($context:ident $(, $argument:expr)* $(,)?) => {{
        let continuation = *$crate::HookChain::fp_next(&mut $context)
            .expect("hook chain has no continuation");
        unsafe { continuation($($argument,)* $context) }
    }};
}

/// Declares static typed hook chains using function-shaped Rust syntax.
///
/// Backend crates normally wrap this hidden macro so they can supply their
/// terminal resolver while callers only choose `#![order(forward)]` or
/// `#![order(reverse)]`. Hook and context names are inferred from the declared
/// function name in the same way as [`function_table_hook!`]. Mark an entry
/// with `#[trampoline]` when its declared function is used as an ABI detour
/// target. `#[trampoline(abi = "...")]` overrides that entry point's ABI while
/// leaving the chain declaration as the source of its signature.
#[doc(hidden)]
#[macro_export]
macro_rules! __chain_hooks {
    (
        resolve with $resolve:ident;
        #![order($order:ident)]
        $($items:tt)*
    ) => {
        $crate::__chain_hooks!(@items $order, $resolve; $($items)*);
    };

    (@items $order:ident, $resolve:ident;) => {};

    (@items $order:ident, $resolve:ident;
        #[trampoline]
        $($item:tt)*
    ) => {
        $crate::__chain_hooks!(@items $order, $resolve; $($item)*);
    };

    (@items $order:ident, $resolve:ident;
        #[trampoline(abi = $trampoline_abi:literal)]
        #[terminal($($terminal:tt)*)]
        $(#[prefix($($prefix:tt)*)])?
        $(#[infix($($infix:path),* $(,)? )])?
        $(#[postfix($($postfix:tt)*)])?
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),* $(,)?
        ) $(-> $result:ty)?;
        $($remaining:tt)*
    ) => {
        $crate::__chain_hooks!(@items $order, $resolve;
            #[terminal($($terminal)*)]
            $(#[prefix($($prefix)*)])?
            $(#[infix($($infix),*)])?
            $(#[postfix($($postfix)*)])?
            $visibility unsafe extern $trampoline_abi fn $name(
                $($argument: $argument_type),*
            ) $(-> $result)?;
            $($remaining)*
        );
    };

    (@items $order:ident, $resolve:ident;
        #[trampoline(abi = $trampoline_abi:literal)]
        #[terminal($($terminal:tt)*)]
        $(#[prefix($($prefix:tt)*)])?
        $(#[infix($($infix:path),* $(,)? )])?
        $(#[postfix($($postfix:tt)*)])?
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),* $(,)?
        ) $(-> $result:ty)? { $($body:tt)* }
        $($remaining:tt)*
    ) => {
        $crate::__chain_hooks!(@items $order, $resolve;
            #[terminal($($terminal)*)]
            $(#[prefix($($prefix)*)])?
            $(#[infix($($infix),*)])?
            $(#[postfix($($postfix)*)])?
            $visibility unsafe extern $trampoline_abi fn $name(
                $($argument: $argument_type),*
            ) $(-> $result)? { $($body)* }
            $($remaining)*
        );
    };

    (@items $order:ident, $resolve:ident;
        #[export($target:ident)]
        #[terminal($($terminal:tt)*)]
        $(#[prefix($($prefix:tt)*)])?
        $(#[infix($($infix:path),* $(,)? )])?
        $(#[postfix($($postfix:tt)*)])?
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),* $(,)?
        ) $(-> $result:ty)?;
        $($remaining:tt)*
    ) => {
        $crate::__chain_hooks!(@items $order, $resolve;
            #[terminal($($terminal)*)]
            $(#[prefix($($prefix)*)])?
            $(#[infix($($infix),*)])?
            $(#[postfix($($postfix)*)])?
            $visibility unsafe extern $abi fn $name(
                $($argument: $argument_type),*
            ) $(-> $result)?;
        );
        #[unsafe(no_mangle)]
        pub unsafe extern $abi fn $target(
            $($argument: $argument_type),*
        ) $(-> $result)? {
            unsafe { $name($($argument),*) }
        }
        $crate::__chain_hooks!(@items $order, $resolve; $($remaining)*);
    };

    (@items $order:ident, $resolve:ident;
        #[export($target:ident)]
        #[terminal($($terminal:tt)*)]
        $(#[prefix($($prefix:tt)*)])?
        $(#[infix($($infix:path),* $(,)? )])?
        $(#[postfix($($postfix:tt)*)])?
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),* $(,)?
        ) $(-> $result:ty)? { $($body:tt)* }
        $($remaining:tt)*
    ) => {
        $crate::__chain_hooks!(@items $order, $resolve;
            #[terminal($($terminal)*)]
            $(#[prefix($($prefix)*)])?
            $(#[infix($($infix),*)])?
            $(#[postfix($($postfix)*)])?
            $visibility unsafe extern $abi fn $name(
                $($argument: $argument_type),*
            ) $(-> $result)? { $($body)* }
        );
        #[unsafe(no_mangle)]
        pub unsafe extern $abi fn $target(
            $($argument: $argument_type),*
        ) $(-> $result)? {
            unsafe { $name($($argument),*) }
        }
        $crate::__chain_hooks!(@items $order, $resolve; $($remaining)*);
    };

    // These two entry arms stay separate because a `ty` fragment cannot be
    // followed by an arbitrary token-tree fragment in `macro_rules!`.
    (@items $order:ident, $resolve:ident;
        #[terminal($($terminal:tt)*)]
        $(#[prefix($($prefix:tt)*)])?
        $(#[infix($($infix:path),* $(,)? )])?
        $(#[postfix($($postfix:tt)*)])?
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),* $(,)?
        ) $(-> $result:ty)?;
        $($remaining:tt)*
    ) => {
        $crate::__chain_hooks!(@normalize_terminal $order, $resolve;
            [$($terminal)*]
            [$(prefix($($prefix)*))?]
            [$(infix($($infix),*))?]
            [$(postfix($($postfix)*))?]
            $visibility unsafe extern $abi fn $name(
                $($argument: $argument_type),*
            ) $(-> $result)? { no context; }
            then { $($remaining)* }
        );
    };

    (@items $order:ident, $resolve:ident;
        #[terminal($($terminal:tt)*)]
        $(#[prefix($($prefix:tt)*)])?
        $(#[infix($($infix:path),* $(,)? )])?
        $(#[postfix($($postfix:tt)*)])?
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),* $(,)?
        ) $(-> $result:ty)? { $($body:tt)* }
        $($remaining:tt)*
    ) => {
        $crate::__chain_hooks!(@normalize_terminal $order, $resolve;
            [$($terminal)*]
            [$(prefix($($prefix)*))?]
            [$(infix($($infix),*))?]
            [$(postfix($($postfix)*))?]
            $visibility unsafe extern $abi fn $name(
                $($argument: $argument_type),*
            ) $(-> $result)? { $($body)* }
            then { $($remaining)* }
        );
    };

    (@normalize_terminal $order:ident, $resolve:ident;
        [$resolver_argument:ident, $symbol:literal: $module:ident::$function_type:ident,
            fallback = $fallback:expr $(,)?]
        $($item:tt)*
    ) => {
        $crate::__chain_hooks!(@finish_item $order, $resolve;
            [resolve with $resolver_argument, $symbol: $module::$function_type -> $fallback;]
            $($item)*
        );
    };

    (@normalize_terminal $order:ident, $resolve:ident;
        [$resolver_argument:ident, $symbol:literal: $module:ident::$function_type:ident]
        $($item:tt)*
    ) => {
        $crate::__chain_hooks!(@finish_item $order, $resolve;
            [resolve with $resolver_argument, $symbol: $module::$function_type;]
            $($item)*
        );
    };

    (@normalize_terminal $order:ident, $resolve:ident;
        [detour = $root:ident $(::$segment:ident)* . $field:ident,
            fallback = $fallback:expr $(,)?]
        $($item:tt)*
    ) => {
        $crate::__chain_hooks!(@finish_item $order, $resolve;
            [resolve with detour $root $(::$segment)*, $field -> $fallback;]
            $($item)*
        );
    };

    (@normalize_terminal $order:ident, $resolve:ident;
        [detour = $detour:path, fallback = $fallback:expr $(,)?]
        $($item:tt)*
    ) => {
        $crate::__chain_hooks!(@finish_item $order, $resolve;
            [resolve with detour $detour, fallback $fallback;]
            $($item)*
        );
    };

    (@normalize_terminal $order:ident, $resolve:ident;
        [$resolver_argument:ident: $resolver_type:ty]
        $($item:tt)*
    ) => {
        $crate::__chain_hooks!(@finish_item $order, $resolve;
            [resolve with $resolver_argument: $resolver_type;]
            $($item)*
        );
    };

    (@normalize_terminal $order:ident, $resolve:ident;
        [$terminal:path]
        $($item:tt)*
    ) => {
        $crate::__chain_hooks!(@finish_item $order, $resolve;
            [resolve with $terminal;]
            $($item)*
        );
    };

    (@finish_item $order:ident, $resolve:ident;
        [$($resolve_tokens:tt)*]
        [$($prefix:tt)*]
        [$($infix:tt)*]
        [$($postfix:tt)*]
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),*
        ) $(-> $result:ty)? { $($body:tt)* }
        then { $($remaining:tt)* }
    ) => {
        $crate::__chain_hooks! {
            @emit $order, $resolve;
            [$($resolve_tokens)*]
            [$($prefix)*]
            [$($infix)*]
            [$($postfix)*]
            $visibility unsafe extern $abi fn $name(
                $($argument: $argument_type),*
            ) $(-> $result)? { $($body)* }
        }
        $crate::__chain_hooks!(@items $order, $resolve; $($remaining)*);
    };

    (@emit $order:ident, $resolve:ident;
        [$($resolve_tokens:tt)*]
        [$($prefix:tt)*]
        [$($infix:tt)*]
        [$($postfix:tt)*]
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),*
        ) -> $result:ty
        { no context; }
    ) => {
        $crate::__chain_hooks!(@generate $order, $resolve;
            [$($resolve_tokens)*] [$($prefix)*] [$($infix)*] [$($postfix)*]
            $visibility unsafe extern $abi fn $name($($argument: $argument_type),*) -> $result;
            callback_context no;
            fields {}
        );
    };

    (@emit $order:ident, $resolve:ident;
        [$($resolve_tokens:tt)*]
        [$($prefix:tt)*]
        [$($infix:tt)*]
        [$($postfix:tt)*]
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),*
        ) -> $result:ty
        { context struct { $($field:ident: $field_type:ty = $field_value:expr),* $(,)? } }
    ) => {
        $crate::__chain_hooks!(@generate $order, $resolve;
            [$($resolve_tokens)*] [$($prefix)*] [$($infix)*] [$($postfix)*]
            $visibility unsafe extern $abi fn $name($($argument: $argument_type),*) -> $result;
            callback_context yes;
            fields { $($field: $field_type = $field_value),* }
        );
    };

    (@emit $order:ident, $resolve:ident;
        [$($resolve_tokens:tt)*]
        [$($prefix:tt)*]
        [$($infix:tt)*]
        [$($postfix:tt)*]
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),*
        )
        $body:tt
    ) => {
        $crate::__chain_hooks!(@emit $order, $resolve;
            [$($resolve_tokens)*] [$($prefix)*] [$($infix)*] [$($postfix)*]
            $visibility unsafe extern $abi fn $name($($argument: $argument_type),*) -> ()
            $body
        );
    };

    (@generate $order:ident, $resolve:ident;
        [$($resolve_tokens:tt)*]
        [$($prefix:tt)*]
        [$($infix:tt)*]
        [$($postfix:tt)*]
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),*
        ) -> $result:ty;
        callback_context $callback_context:ident;
        fields { $($field:ident: $field_type:ty = $field_value:expr),* }
    ) => { $crate::__paste! {
        $visibility type [<$name:camel Hook>] = unsafe fn(
            $($argument_type,)*
            [<$name:camel Context>]<'_>,
        ) -> $result;

        $visibility struct [<$name:camel Context>]<'a> {
            hooks: &'a [[<$name:camel Hook>]],
            cursor: usize,
            reverse: bool,
            terminal: &'a [<$name:camel Hook>],
            $($visibility $field: &'a $field_type,)*
        }

        impl<'a> $crate::HookChain<'a, [<$name:camel Hook>]>
            for [<$name:camel Context>]<'a>
        {
            fn fp_next(&mut self) -> Option<&'a [<$name:camel Hook>]> {
                if self.reverse {
                    if self.cursor == 0 {
                        Some(self.terminal)
                    } else {
                        self.cursor -= 1;
                        Some(&self.hooks[self.cursor])
                    }
                } else if let Some(hook) = self.hooks.get(self.cursor) {
                    self.cursor += 1;
                    Some(hook)
                } else {
                    Some(self.terminal)
                }
            }
        }

        #[cfg(test)]
        impl<'a> [<$name:camel Context>]<'a> {
            #[allow(dead_code)]
            $visibility fn from_fields($($field: &'a $field_type),*) -> Self {
                #[allow(unused_variables)]
                unsafe fn unavailable(
                    $($argument: $argument_type,)*
                    _context: [<$name:camel Context>]<'_>,
                ) -> $result {
                    panic!("test hook context has no continuation")
                }
                const EMPTY: &[[<$name:camel Hook>]] = &[];
                let terminal: &'static [<$name:camel Hook>] = &(unavailable as [<$name:camel Hook>]);
                Self {
                    hooks: EMPTY,
                    cursor: 0,
                    reverse: false,
                    terminal,
                    $($field,)*
                }
            }
        }

        unsafe fn [<__chain_dispatch_ $name>](
            $($argument: $argument_type),*
        ) -> $result {
            unsafe fn chain_terminal(
                $($argument: $argument_type,)*
                _context: [<$name:camel Context>]<'_>,
            ) -> $result {
                ($resolve! {
                    ($($argument),*);
                    $($resolve_tokens)*
                })()
            }

            $(let $field = $field_value;)*
            let terminal: [<$name:camel Hook>] = chain_terminal;
            let hooks: &[[<$name:camel Hook>]] =
                $crate::__chain_hooks!(@infix_slice $($infix)*);
            let reverse = $crate::__chain_hooks!(@is_reverse $order);
            let cursor = if reverse { hooks.len() } else { 0 };
            let context = [<$name:camel Context>] {
                hooks,
                cursor,
                reverse,
                terminal: &terminal,
                $($field: &$field,)*
            };

            let prefix_hooks: &[fn(
                $($argument_type,)*
                &[<$name:camel Context>]<'_>,
            )] = $crate::__chain_hooks!(@prefix_slice
                [$(($argument, $argument_type))*]
                [[<$name:camel Context>]]
                [$callback_context]
                [];
                $($prefix)*
            );
            if reverse {
                for hook in prefix_hooks.iter().rev() {
                    hook($($argument,)* &context);
                }
            } else {
                for hook in prefix_hooks {
                    hook($($argument,)* &context);
                }
            }

            let mut chain = context;
            let continuation = *$crate::HookChain::fp_next(&mut chain)
                .expect("hook chain has no continuation");
            let result = unsafe { continuation($($argument,)* chain) };

            let context = [<$name:camel Context>] {
                hooks: &[],
                cursor: 0,
                reverse: false,
                terminal: &terminal,
                $($field: &$field,)*
            };
            let postfix_hooks: &[fn(
                $($argument_type,)*
                $result,
                &[<$name:camel Context>]<'_>,
            )] = $crate::__chain_hooks!(@postfix_slice
                [$(($argument, $argument_type))*]
                [$result]
                [[<$name:camel Context>]]
                [$callback_context]
                [];
                $($postfix)*
            );
            if reverse {
                for hook in postfix_hooks.iter().rev() {
                    hook($($argument,)* result, &context);
                }
            } else {
                for hook in postfix_hooks {
                    hook($($argument,)* result, &context);
                }
            }
            result
        }

        $visibility unsafe extern $abi fn $name(
            $($argument: $argument_type),*
        ) -> $result {
            unsafe { [<__chain_dispatch_ $name>]($($argument),*) }
        }
    }};

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
        $crate::__chain_hooks!(@prefix_adapters
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
        $crate::__chain_hooks!(@prefix_adapters
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
        $crate::__chain_hooks!(@prefix_adapters
            [$(($argument, $argument_type))*]
            [$context]
            [$callback_context]
            [$($adapter,)*
                |$($argument: $argument_type,)* context: &$context<'_>| {
                    let _ = ($(&$argument,)*);
                    $crate::__chain_hooks!(@call_prefix [$callback_context] context;
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
        $crate::__chain_hooks!(@postfix_adapters
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
        $crate::__chain_hooks!(@postfix_adapters
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
        $crate::__chain_hooks!(@postfix_adapters
            [$(($argument, $argument_type))*]
            [$result_type]
            [$context]
            [$callback_context]
            [$($adapter,)*
                |$($argument: $argument_type,)* result: $result_type, context: &$context<'_>| {
                    let _ = ($(&$argument,)*);
                    $crate::__chain_hooks!(@call_post_mode [$callback_context]
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
        $crate::__chain_hooks!(@call_post $result; []; [$($hook)::+]; []; $($argument),*)
    }};
    (@call_post_mode [yes] $result:ident; $context:expr; [$($hook:ident)::+];
        $($argument:ident),*
    ) => {
        $crate::__chain_hooks!(@call_post $result; [$context,]; [$($hook)::+]; [];
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
        $crate::__chain_hooks!(@call_post $result; [$($suffix,)*]; [$($hook)::+];
            [$($mapped,)* $result,]; $($($remaining),*)?
        )
    };
    (@call_post $result:ident; [$($suffix:expr,)*]; [$($hook:ident)::+]; [$($mapped:expr,)*];
        $argument:ident $(, $($remaining:ident),*)?
    ) => {
        $crate::__chain_hooks!(@call_post $result; [$($suffix,)*]; [$($hook)::+];
            [$($mapped,)* $argument,]; $($($remaining),*)?
        )
    };
}

#[cfg(test)]
mod static_chain_tests {
    use std::sync::Mutex;

    static EVENTS: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    static FORWARD_EVENTS: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

    unsafe fn terminal(value: usize) -> usize {
        EVENTS.lock().unwrap().push("terminal");
        value * 2
    }

    unsafe fn forward_terminal(value: usize) -> usize {
        FORWARD_EVENTS.lock().unwrap().push("terminal");
        value * 2
    }

    macro_rules! resolve_terminal {
        (($($argument:ident),*); resolve with $terminal:path;) => {
            || unsafe { $terminal($($argument),*) }
        };
    }

    macro_rules! chain_hooks {
        ($($tokens:tt)*) => {
            crate::__chain_hooks! {
                resolve with resolve_terminal;
                $($tokens)*
            }
        };
    }

    unsafe fn prefix_one(value: usize, context: &OperationContext<'_>) {
        assert_eq!(*context.offset, 3);
        assert_eq!(value, 4);
        EVENTS.lock().unwrap().push("prefix-one");
    }

    unsafe fn prefix_two(context: &OperationContext<'_>) {
        assert_eq!(*context.offset, 3);
        EVENTS.lock().unwrap().push("prefix-two");
    }

    unsafe fn infix_one(value: usize, mut next: OperationContext<'_>) -> usize {
        EVENTS.lock().unwrap().push("infix-one-enter");
        let result = crate::next!(next, value);
        EVENTS.lock().unwrap().push("infix-one-exit");
        result
    }

    unsafe fn infix_two(value: usize, mut next: OperationContext<'_>) -> usize {
        EVENTS.lock().unwrap().push("infix-two-enter");
        let result = crate::next!(next, value);
        EVENTS.lock().unwrap().push("infix-two-exit");
        result
    }

    unsafe fn postfix_one(result: usize, context: &OperationContext<'_>) {
        assert_eq!(*context.offset, 3);
        assert_eq!(result, 8);
        EVENTS.lock().unwrap().push("postfix-one");
    }

    unsafe fn postfix_two(value: usize, result: usize, _context: &OperationContext<'_>) {
        assert_eq!((value, result), (4, 8));
        EVENTS.lock().unwrap().push("postfix-two");
    }

    unsafe fn forward_infix(value: usize, mut next: ForwardOperationContext<'_>) -> usize {
        FORWARD_EVENTS.lock().unwrap().push("forward-infix");
        crate::next!(next, value)
    }

    unsafe fn forward_prefix() {
        FORWARD_EVENTS.lock().unwrap().push("forward-prefix");
    }

    unsafe fn forward_postfix(result: usize) {
        assert_eq!(result, 10);
        FORWARD_EVENTS.lock().unwrap().push("forward-postfix");
    }

    chain_hooks! {
        #![order(reverse)]

        #[terminal(terminal)]
        #[prefix(prefix_one(value), prefix_two())]
        #[infix(infix_one, infix_two)]
        #[postfix(postfix_one(result), postfix_two(value, result))]
        pub(crate) unsafe extern "system" fn operation(value: usize) -> usize {
            context struct {
                offset: usize = 3,
            }
        }
    }

    chain_hooks! {
        #![order(forward)]

        #[terminal(forward_terminal)]
        #[prefix(forward_prefix())]
        #[infix(forward_infix)]
        #[postfix(forward_postfix(result))]
        pub(crate) unsafe extern "system" fn forward_operation(value: usize) -> usize;
    }

    #[test]
    fn static_chain_uses_reverse_order_in_each_phase() {
        EVENTS.lock().unwrap().clear();
        assert_eq!(unsafe { operation(4) }, 8);
        assert_eq!(
            *EVENTS.lock().unwrap(),
            [
                "prefix-two",
                "prefix-one",
                "infix-two-enter",
                "infix-one-enter",
                "terminal",
                "infix-one-exit",
                "infix-two-exit",
                "postfix-two",
                "postfix-one",
            ]
        );
    }

    #[test]
    fn static_chain_supports_forward_order_and_an_inferred_empty_context() {
        FORWARD_EVENTS.lock().unwrap().clear();
        assert_eq!(unsafe { forward_operation(5) }, 10);
        assert_eq!(
            *FORWARD_EVENTS.lock().unwrap(),
            [
                "forward-prefix",
                "forward-infix",
                "terminal",
                "forward-postfix",
            ]
        );
    }
}
