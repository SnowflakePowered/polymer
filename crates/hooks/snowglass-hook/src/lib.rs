//! Shared typed-hook infrastructure.

/// A continuation over a typed hook chain.
pub trait HookChain<'a, T: 'a> {
    /// Advances to the next registered hook, if one remains.
    fn fp_next(&mut self) -> Option<&'a T>;
}

/// Registration order used while visiting a hook chain.
#[derive(Clone, Copy)]
pub enum ChainOrder {
    Forward,
    Reverse,
}

fn for_each_hook<T>(
    chain: &std::sync::RwLock<Vec<T>>,
    order: ChainOrder,
    mut visit: impl FnMut(&T),
) {
    let chain = chain.read().unwrap_or_else(|error| error.into_inner());
    match order {
        ChainOrder::Forward => chain.iter().for_each(&mut visit),
        ChainOrder::Reverse => chain.iter().rev().for_each(visit),
    }
}

/// Runs every observer before invoking the terminal.
pub fn dispatch_observer_chain<T, R>(
    chain: &std::sync::RwLock<Vec<T>>,
    order: ChainOrder,
    observe: impl FnMut(&T),
    terminal: impl FnOnce() -> R,
) -> R {
    for_each_hook(chain, order, observe);
    terminal()
}

/// Invokes the terminal, passes its result to every post hook, and returns it.
pub fn dispatch_post_chain<T, R: Copy>(
    chain: &std::sync::RwLock<Vec<T>>,
    order: ChainOrder,
    terminal: impl FnOnce() -> R,
    mut post: impl FnMut(&T, R),
) -> R {
    let result = terminal();
    for_each_hook(chain, order, |hook| post(hook, result));
    result
}

/// Declares storage and idempotent registration for one typed hook chain.
#[macro_export]
macro_rules! declare_chain_storage {
    (
        static $chain:ident for $hook:ty;
        $visibility:vis register $register:ident;
    ) => {
        static $chain: std::sync::LazyLock<std::sync::RwLock<Vec<$hook>>> =
            std::sync::LazyLock::new(|| std::sync::RwLock::new(Vec::new()));

        #[allow(dead_code)]
        $visibility fn $register(hook: $hook) {
            let mut chain = $chain.write().unwrap_or_else(|error| error.into_inner());
            if !chain
                .iter()
                .any(|registered| *registered as usize == hook as usize)
            {
                chain.push(hook);
            }
        }
    };
}

/// Declares a forward-only continuation context for a typed hook chain.
#[macro_export]
macro_rules! declare_chain_context {
    ($visibility:vis struct $context:ident for $hook:ty;) => {
        $visibility struct $context<'a> {
            hooks: std::slice::Iter<'a, $hook>,
        }

        impl<'a> $crate::HookChain<'a, $hook> for $context<'a> {
            fn fp_next(&mut self) -> Option<&'a $hook> {
                self.hooks.next()
            }
        }
    };
}

/// Declares a continuation context whose exhausted chain falls through to a
/// terminal hook. The iteration order remains a backend choice.
#[macro_export]
macro_rules! declare_terminal_chain_context {
    (
        $visibility:vis struct $context:ident for $hook:ty;
        forward;
        fields { $($field:ident: $field_type:ty),* $(,)? };
    ) => {
        $visibility struct $context<'a> {
            hooks: std::slice::Iter<'a, $hook>,
            terminal: &'a $hook,
            $(pub(crate) $field: $field_type,)*
        }

        impl<'a> $crate::HookChain<'a, $hook> for $context<'a> {
            fn fp_next(&mut self) -> Option<&'a $hook> {
                Some(self.hooks.next().unwrap_or(self.terminal))
            }
        }
    };
    ($visibility:vis struct $context:ident for $hook:ty; forward;) => {
        $crate::declare_terminal_chain_context! {
            $visibility struct $context for $hook;
            forward;
            fields {};
        }
    };
    (
        $visibility:vis struct $context:ident for $hook:ty;
        reverse;
        fields { $($field:ident: $field_type:ty),* $(,)? };
    ) => {
        $visibility struct $context<'a> {
            hooks: std::iter::Rev<std::slice::Iter<'a, $hook>>,
            terminal: &'a $hook,
            $(pub(crate) $field: $field_type,)*
        }

        impl<'a> $crate::HookChain<'a, $hook> for $context<'a> {
            fn fp_next(&mut self) -> Option<&'a $hook> {
                Some(self.hooks.next().unwrap_or(self.terminal))
            }
        }
    };
    ($visibility:vis struct $context:ident for $hook:ty; reverse;) => {
        $crate::declare_terminal_chain_context! {
            $visibility struct $context for $hook;
            reverse;
            fields {};
        }
    };
}

/// Dispatches the first hook in a terminal-backed chain and constructs the
/// continuation context shared by all remaining hooks.
#[macro_export]
macro_rules! dispatch_terminal_chain {
    (
        forward chain $chain:ident;
        context $context:ident;
        terminal $terminal:ident;
        fields { $($field:ident),* $(,)? };
        args ($($argument:expr),* $(,)?);
    ) => {{
        let chain = $chain.read().unwrap_or_else(|error| error.into_inner());
        let mut hooks = chain.iter();
        let next = hooks.next().unwrap_or(&$terminal);
        unsafe {
            next(
                $($argument,)*
                $context {
                    hooks,
                    terminal: &$terminal,
                    $($field,)*
                },
            )
        }
    }};
    (
        reverse chain $chain:ident;
        context $context:ident;
        terminal $terminal:ident;
        fields { $($field:ident),* $(,)? };
        args ($($argument:expr),* $(,)?);
    ) => {{
        let chain = $chain.read().unwrap_or_else(|error| error.into_inner());
        let mut hooks = chain.iter().rev();
        let next = hooks.next().unwrap_or(&$terminal);
        unsafe {
            next(
                $($argument,)*
                $context {
                    hooks,
                    terminal: &$terminal,
                    $($field,)*
                },
            )
        }
    }};
}

/// Declares a thin system-ABI trampoline forwarding directly to a dispatcher.
#[macro_export]
macro_rules! trampoline {
    (
        $(#[$attribute:meta])*
        $visibility:vis unsafe extern "system" fn $name:ident(
            $($argument:ident: $argument_type:ty),* $(,)?
        ) -> $result:ty => $dispatch:path;
    ) => {
        $(#[$attribute])*
        $visibility unsafe extern "system" fn $name(
            $($argument: $argument_type),*
        ) -> $result {
            unsafe { $dispatch($($argument),*) }
        }
    };
}

/// Emits an ABI export for externally visible chains, or nothing for internal
/// chains.
#[doc(hidden)]
#[macro_export]
macro_rules! emit_chain_export {
    (
        extern;
        target $target:ident;
        dispatch $dispatch:ident;
        abi ($($abi_arg:ident: $abi_arg_type:ty),*);
        result $result:ty;
    ) => {
        $crate::trampoline! {
            #[unsafe(no_mangle)]
            pub unsafe extern "system" fn $target(
                $($abi_arg: $abi_arg_type),*
            ) -> $result => $dispatch;
        }
    };
    (
        internal;
        target $target:ident;
        dispatch $dispatch:ident;
        abi ($($abi_arg:ident: $abi_arg_type:ty),*);
        result $result:ty;
    ) => {};
}

/// Declares a backend observer-chain expansion macro. Backends provide only
/// terminal resolution, export mode, and iteration order.
#[macro_export]
macro_rules! decl_observer_chain {
    (
        $visibility:vis $name:ident;
        order $order:path;
        resolve with $resolve:ident;
        export $export:ident;
        $dollar:tt
    ) => {
        macro_rules! $name {
            (
                $dollar chain_visibility:vis $dollar dispatch:ident, $dollar register:ident;
                target $dollar target:ident;
                chain $dollar chain:ident, context $dollar context:ident;
                hook $dollar hook:ident($dollar($dollar hook_arg:ident: $dollar hook_arg_type:ty),* $dollar(,)?)
                    -> $dollar result_name:ident: $dollar result:ty;
                $dollar($dollar tokens:tt)*
            ) => {
                $name! {
                    $dollar chain_visibility $dollar dispatch, $dollar register;
                    target $dollar target;
                    chain $dollar chain, context $dollar context;
                    hook $dollar hook($dollar($dollar hook_arg: $dollar hook_arg_type),*)
                        -> $dollar result;
                    $dollar($dollar tokens)*
                }
            };
            (
                $dollar chain_visibility:vis $dollar dispatch:ident, $dollar register:ident;
                target $dollar target:ident;
                chain $dollar chain:ident, context $dollar context:ident;
                hook $dollar hook:ident($dollar($dollar hook_arg:ident: $dollar hook_arg_type:ty),* $dollar(,)?)
                    -> $dollar result:ty;
                abi ($dollar($dollar abi_arg:ident: $dollar abi_arg_type:ty),* $dollar(,)?);
                $dollar($dollar resolve_tokens:tt)*
            ) => {
                type $dollar hook = unsafe fn($dollar($dollar hook_arg_type),*);
                $crate::declare_chain_storage! {
                    static $dollar chain for $dollar hook;
                    $dollar chain_visibility register $dollar register;
                }
                $dollar chain_visibility unsafe fn $dollar dispatch(
                    $dollar($dollar abi_arg: $dollar abi_arg_type),*
                ) -> $dollar result {
                    crate::register_hooks();
                    $crate::dispatch_observer_chain(
                        &$dollar chain,
                        $order,
                        |hook| unsafe { hook($dollar($dollar hook_arg),*) },
                        $resolve! {
                            ($dollar($dollar abi_arg),*);
                            $dollar($dollar resolve_tokens)*
                        },
                    )
                }
                $crate::emit_chain_export! {
                    $export;
                    target $dollar target;
                    dispatch $dollar dispatch;
                    abi ($dollar($dollar abi_arg: $dollar abi_arg_type),*);
                    result $dollar result;
                }
            };
            (
                $dollar chain_visibility:vis $dollar dispatch:ident, $dollar register:ident;
                target $dollar target:ident;
                chain $dollar chain:ident, context $dollar context:ident;
                hook $dollar hook:ident($dollar($dollar hook_arg:ident: $dollar hook_arg_type:ty),* $dollar(,)?)
                    -> $dollar result:ty;
                resolve with $dollar($dollar resolve_tokens:tt)*
            ) => {
                $name! {
                    $dollar chain_visibility $dollar dispatch, $dollar register;
                    target $dollar target;
                    chain $dollar chain, context $dollar context;
                    hook $dollar hook($dollar($dollar hook_arg: $dollar hook_arg_type),*)
                        -> $dollar result;
                    abi ($dollar($dollar hook_arg: $dollar hook_arg_type),*);
                    resolve with $dollar($dollar resolve_tokens)*
                }
            };
            (
                $dollar chain_visibility:vis $dollar dispatch:ident, $dollar register:ident;
                target $dollar target:ident;
                chain $dollar chain:ident, context $dollar context:ident;
                hook $dollar hook:ident($dollar($dollar hook_arg:ident: $dollar hook_arg_type:ty),* $dollar(,)?);
                abi ($dollar($dollar abi_arg:ident: $dollar abi_arg_type:ty),* $dollar(,)?);
                $dollar($dollar resolve_tokens:tt)*
            ) => {
                $name! {
                    $dollar chain_visibility $dollar dispatch, $dollar register;
                    target $dollar target;
                    chain $dollar chain, context $dollar context;
                    hook $dollar hook($dollar($dollar hook_arg: $dollar hook_arg_type),*) -> ();
                    abi ($dollar($dollar abi_arg: $dollar abi_arg_type),*);
                    $dollar($dollar resolve_tokens)*
                }
            };
            (
                $dollar chain_visibility:vis $dollar dispatch:ident, $dollar register:ident;
                target $dollar target:ident;
                chain $dollar chain:ident, context $dollar context:ident;
                hook $dollar hook:ident($dollar($dollar hook_arg:ident: $dollar hook_arg_type:ty),* $dollar(,)?);
                resolve with $dollar($dollar resolve_tokens:tt)*
            ) => {
                $name! {
                    $dollar chain_visibility $dollar dispatch, $dollar register;
                    target $dollar target;
                    chain $dollar chain, context $dollar context;
                    hook $dollar hook($dollar($dollar hook_arg: $dollar hook_arg_type),*);
                    abi ($dollar($dollar hook_arg: $dollar hook_arg_type),*);
                    resolve with $dollar($dollar resolve_tokens)*
                }
            };
        }
        $visibility use $name;
    };
}

/// Declares a backend post-chain expansion macro. The generated dispatcher
/// invokes the terminal first and then visits each hook with its result.
#[macro_export]
macro_rules! decl_post_chain {
    (
        $visibility:vis $name:ident;
        order $order:path;
        resolve with $resolve:ident;
        export $export:ident;
        $dollar:tt
    ) => {
        macro_rules! $name {
            (
                $dollar chain_visibility:vis $dollar dispatch:ident, $dollar register:ident;
                target $dollar target:ident;
                chain $dollar chain:ident, context $dollar context:ident;
                hook $dollar hook:ident($dollar($dollar hook_arg:ident: $dollar hook_arg_type:ty),* $dollar(,)?)
                    -> $dollar result_argument:ident: $dollar result:ty;
                abi ($dollar($dollar abi_arg:ident: $dollar abi_arg_type:ty),* $dollar(,)?);
                $dollar($dollar resolve_tokens:tt)*
            ) => {
                type $dollar hook = unsafe fn($dollar($dollar hook_arg_type,)* $dollar result);
                $crate::declare_chain_storage! {
                    static $dollar chain for $dollar hook;
                    $dollar chain_visibility register $dollar register;
                }
                $dollar chain_visibility unsafe fn $dollar dispatch(
                    $dollar($dollar abi_arg: $dollar abi_arg_type),*
                ) -> $dollar result {
                    crate::register_hooks();
                    $crate::dispatch_post_chain(
                        &$dollar chain,
                        $order,
                        $resolve! {
                            ($dollar($dollar abi_arg),*);
                            $dollar($dollar resolve_tokens)*
                        },
                        |hook, $dollar result_argument| unsafe {
                            hook($dollar($dollar hook_arg,)* $dollar result_argument)
                        },
                    )
                }
                $crate::emit_chain_export! {
                    $export;
                    target $dollar target;
                    dispatch $dollar dispatch;
                    abi ($dollar($dollar abi_arg: $dollar abi_arg_type),*);
                    result $dollar result;
                }
            };
            (
                $dollar chain_visibility:vis $dollar dispatch:ident, $dollar register:ident;
                target $dollar target:ident;
                chain $dollar chain:ident, context $dollar context:ident;
                hook $dollar hook:ident($dollar($dollar hook_arg:ident: $dollar hook_arg_type:ty),* $dollar(,)?)
                    -> $dollar result_argument:ident: $dollar result:ty;
                resolve with $dollar($dollar resolve_tokens:tt)*
            ) => {
                $name! {
                    $dollar chain_visibility $dollar dispatch, $dollar register;
                    target $dollar target;
                    chain $dollar chain, context $dollar context;
                    hook $dollar hook($dollar($dollar hook_arg: $dollar hook_arg_type),*)
                        -> $dollar result_argument: $dollar result;
                    abi ($dollar($dollar hook_arg: $dollar hook_arg_type),*);
                    resolve with $dollar($dollar resolve_tokens)*
                }
            };
            (
                $dollar chain_visibility:vis $dollar dispatch:ident, $dollar register:ident;
                target $dollar target:ident;
                chain $dollar chain:ident, context $dollar context:ident;
                hook $dollar hook:ident($dollar($dollar hook_arg:ident: $dollar hook_arg_type:ty),* $dollar(,)?)
                    -> $dollar result:ty;
                $dollar($dollar tokens:tt)*
            ) => {
                $name! {
                    $dollar chain_visibility $dollar dispatch, $dollar register;
                    target $dollar target;
                    chain $dollar chain, context $dollar context;
                    hook $dollar hook($dollar($dollar hook_arg: $dollar hook_arg_type),*)
                        -> result: $dollar result;
                    $dollar($dollar tokens)*
                }
            };
            (
                $dollar chain_visibility:vis $dollar dispatch:ident, $dollar register:ident;
                target $dollar target:ident;
                chain $dollar chain:ident, context $dollar context:ident;
                hook $dollar hook:ident($dollar($dollar hook_arg:ident: $dollar hook_arg_type:ty),* $dollar(,)?);
                abi ($dollar($dollar abi_arg:ident: $dollar abi_arg_type:ty),* $dollar(,)?);
                $dollar($dollar resolve_tokens:tt)*
            ) => {
                type $dollar hook = unsafe fn($dollar($dollar hook_arg_type),*);
                $crate::declare_chain_storage! {
                    static $dollar chain for $dollar hook;
                    $dollar chain_visibility register $dollar register;
                }
                $dollar chain_visibility unsafe fn $dollar dispatch(
                    $dollar($dollar abi_arg: $dollar abi_arg_type),*
                ) {
                    crate::register_hooks();
                    $crate::dispatch_post_chain(
                        &$dollar chain,
                        $order,
                        $resolve! {
                            ($dollar($dollar abi_arg),*);
                            $dollar($dollar resolve_tokens)*
                        },
                        |hook, ()| unsafe { hook($dollar($dollar hook_arg),*) },
                    )
                }
                $crate::emit_chain_export! {
                    $export;
                    target $dollar target;
                    dispatch $dollar dispatch;
                    abi ($dollar($dollar abi_arg: $dollar abi_arg_type),*);
                    result ();
                }
            };
            (
                $dollar chain_visibility:vis $dollar dispatch:ident, $dollar register:ident;
                target $dollar target:ident;
                chain $dollar chain:ident, context $dollar context:ident;
                hook $dollar hook:ident($dollar($dollar hook_arg:ident: $dollar hook_arg_type:ty),* $dollar(,)?);
                resolve with $dollar($dollar resolve_tokens:tt)*
            ) => {
                $name! {
                    $dollar chain_visibility $dollar dispatch, $dollar register;
                    target $dollar target;
                    chain $dollar chain, context $dollar context;
                    hook $dollar hook($dollar($dollar hook_arg: $dollar hook_arg_type),*);
                    abi ($dollar($dollar hook_arg: $dollar hook_arg_type),*);
                    resolve with $dollar($dollar resolve_tokens)*
                }
            };
        }
        $visibility use $name;
    };
}

/// Defines the common storage, context, dispatcher, and export plumbing for
/// an around chain after a backend has translated its terminal resolver.
#[doc(hidden)]
#[macro_export]
macro_rules! define_around_chain {
    (
        $visibility:vis $dispatch:ident, $register:ident;
        order $order:ident;
        target $target:ident;
        chain $chain:ident, context $context:ident;
        hook $hook:ident($($hook_arg:ident: $hook_arg_type:ty),* $(,)?) -> $result:ty;
        abi ($($abi_arg:ident: $abi_arg_type:ty),* $(,)?);
        prepare { $($prepare:tt)* }
        fields { $($field:ident: $field_type:ty = $field_value:expr),* $(,)? }
        terminal $terminal:expr;
        export $export:ident;
    ) => {
        $visibility type $hook = for<'a> unsafe fn(
            $($hook_arg_type,)*
            $context<'a>,
        ) -> $result;

        $crate::declare_terminal_chain_context! {
            $visibility struct $context for $hook;
            $order;
            fields { $($field: $field_type),* };
        }

        $crate::declare_chain_storage! {
            static $chain for $hook;
            $visibility register $register;
        }

        $visibility unsafe fn $dispatch(
            $($abi_arg: $abi_arg_type),*
        ) -> $result {
            crate::register_hooks();
            $($prepare)*
            $(let $field = $field_value;)*
            let terminal: $hook = $terminal;
            $crate::dispatch_terminal_chain! {
                $order chain $chain;
                context $context;
                terminal terminal;
                fields { $($field),* };
                args ($($hook_arg),*);
            }
        }

        $crate::emit_chain_export! {
            $export;
            target $target;
            dispatch $dispatch;
            abi ($($abi_arg: $abi_arg_type),*);
            result $result;
        }
    };
}

/// Declares a backend around-chain expansion macro. The backend resolver only
/// translates its terminal syntax into `define_around_chain!` inputs.
#[macro_export]
macro_rules! decl_around_chain {
    (
        $visibility:vis $name:ident;
        order $order:ident;
        resolve with $resolve:ident;
        export $export:ident;
        $dollar:tt
    ) => {
        macro_rules! $name {
            (
                $dollar chain_visibility:vis $dollar dispatch:ident, $dollar register:ident;
                target $dollar target:ident;
                chain $dollar chain:ident, context $dollar context:ident;
                hook $dollar hook:ident($dollar($dollar hook_arg:ident: $dollar hook_arg_type:ty),* $dollar(,)?)
                    -> $dollar result_name:ident: $dollar result:ty;
                $dollar($dollar tokens:tt)*
            ) => {
                $name! {
                    $dollar chain_visibility $dollar dispatch, $dollar register;
                    target $dollar target;
                    chain $dollar chain, context $dollar context;
                    hook $dollar hook($dollar($dollar hook_arg: $dollar hook_arg_type),*)
                        -> $dollar result;
                    $dollar($dollar tokens)*
                }
            };
            (
                $dollar chain_visibility:vis $dollar dispatch:ident, $dollar register:ident;
                target $dollar target:ident;
                chain $dollar chain:ident, context $dollar context:ident;
                hook $dollar hook:ident($dollar($dollar hook_arg:ident: $dollar hook_arg_type:ty),* $dollar(,)?)
                    -> $dollar result:ty;
                abi ($dollar($dollar abi_arg:ident: $dollar abi_arg_type:ty),* $dollar(,)?);
                $dollar($dollar resolve_tokens:tt)*
            ) => {
                $resolve! {
                    $dollar chain_visibility $dollar dispatch, $dollar register;
                    order $order;
                    target $dollar target;
                    chain $dollar chain, context $dollar context;
                    hook $dollar hook($dollar($dollar hook_arg: $dollar hook_arg_type),*)
                        -> $dollar result;
                    abi ($dollar($dollar abi_arg: $dollar abi_arg_type),*);
                    export $export;
                    $dollar($dollar resolve_tokens)*
                }
            };
            (
                $dollar chain_visibility:vis $dollar dispatch:ident, $dollar register:ident;
                target $dollar target:ident;
                chain $dollar chain:ident, context $dollar context:ident;
                hook $dollar hook:ident($dollar($dollar hook_arg:ident: $dollar hook_arg_type:ty),* $dollar(,)?)
                    -> $dollar result:ty;
                resolve with $dollar($dollar resolve_tokens:tt)*
            ) => {
                $name! {
                    $dollar chain_visibility $dollar dispatch, $dollar register;
                    target $dollar target;
                    chain $dollar chain, context $dollar context;
                    hook $dollar hook($dollar($dollar hook_arg: $dollar hook_arg_type),*)
                        -> $dollar result;
                    abi ($dollar($dollar hook_arg: $dollar hook_arg_type),*);
                    resolve with $dollar($dollar resolve_tokens)*
                }
            };
        }
        $visibility use $name;
    };
}

/// Declares all backend chain modes and a `chain!` frontend which routes to
/// them. Each mode supplies only its iteration order, resolver, and exporter.
///
/// The dollar token is supplied by the caller because stable `macro_rules!`
/// cannot otherwise escape metavariables while defining another macro.
#[macro_export]
macro_rules! define_chain_macro {
    (
        $visibility:vis $name:ident => { $($modes:tt)* },
        $dollar:tt
    ) => {
        $crate::define_chain_macro! {
            @observer $visibility $name, $dollar;
            $($modes)*
        }
    };
    (
        @observer $visibility:vis $name:ident, $dollar:tt;
        extern observer {
            order $order:path;
            resolve with $resolve:ident;
        }
        $($remaining:tt)*
    ) => {
        $crate::define_chain_macro! {
            @post $visibility $name, $dollar;
            observer [extern, $order, $resolve];
            $($remaining)*
        }
    };
    (
        @observer $visibility:vis $name:ident, $dollar:tt;
        observer {
            order $order:path;
            resolve with $resolve:ident;
        }
        $($remaining:tt)*
    ) => {
        $crate::define_chain_macro! {
            @post $visibility $name, $dollar;
            observer [internal, $order, $resolve];
            $($remaining)*
        }
    };
    (
        @post $visibility:vis $name:ident, $dollar:tt;
        observer [$observer_export:ident, $observer_order:path, $observer_resolve:ident];
        extern post {
            order $order:path;
            resolve with $resolve:ident;
        }
        $($remaining:tt)*
    ) => {
        $crate::define_chain_macro! {
            @around $visibility $name, $dollar;
            observer [$observer_export, $observer_order, $observer_resolve];
            post [extern, $order, $resolve];
            $($remaining)*
        }
    };
    (
        @post $visibility:vis $name:ident, $dollar:tt;
        observer [$observer_export:ident, $observer_order:path, $observer_resolve:ident];
        post {
            order $order:path;
            resolve with $resolve:ident;
        }
        $($remaining:tt)*
    ) => {
        $crate::define_chain_macro! {
            @around $visibility $name, $dollar;
            observer [$observer_export, $observer_order, $observer_resolve];
            post [internal, $order, $resolve];
            $($remaining)*
        }
    };
    (
        @around $visibility:vis $name:ident, $dollar:tt;
        observer [$observer_export:ident, $observer_order:path, $observer_resolve:ident];
        post [$post_export:ident, $post_order:path, $post_resolve:ident];
        extern around {
            order $around_order:ident;
            resolve with $around_resolve:ident;
        }
        $(,)?
    ) => {
        $crate::define_chain_macro! {
            @define $visibility $name, $dollar;
            observer [$observer_export, $observer_order, $observer_resolve];
            post [$post_export, $post_order, $post_resolve];
            around [extern, $around_order, $around_resolve];
        }
    };
    (
        @around $visibility:vis $name:ident, $dollar:tt;
        observer [$observer_export:ident, $observer_order:path, $observer_resolve:ident];
        post [$post_export:ident, $post_order:path, $post_resolve:ident];
        around {
            order $around_order:ident;
            resolve with $around_resolve:ident;
        }
        $(,)?
    ) => {
        $crate::define_chain_macro! {
            @define $visibility $name, $dollar;
            observer [$observer_export, $observer_order, $observer_resolve];
            post [$post_export, $post_order, $post_resolve];
            around [internal, $around_order, $around_resolve];
        }
    };
    (
        @define $visibility:vis $name:ident, $dollar:tt;
        observer [$observer_export:ident, $observer_order:path, $observer_resolve:ident];
        post [$post_export:ident, $post_order:path, $post_resolve:ident];
        around [$around_export:ident, $around_order:ident, $around_resolve:ident];
    ) => {
        $crate::decl_observer_chain! {
            $visibility observer_chain;
            order $observer_order;
            resolve with $observer_resolve;
            export $observer_export;
            $dollar
        }

        $crate::decl_post_chain! {
            $visibility post_chain;
            order $post_order;
            resolve with $post_resolve;
            export $post_export;
            $dollar
        }

        $crate::decl_around_chain! {
            $visibility around_chain;
            order $around_order;
            resolve with $around_resolve;
            export $around_export;
            $dollar
        }

        macro_rules! $name {
            (
                $dollar chain_visibility:vis $dollar dispatch:ident, $dollar register:ident;
                observe $dollar target:ident;
                $dollar($dollar tokens:tt)*
            ) => {
                observer_chain! {
                    $dollar chain_visibility $dollar dispatch, $dollar register;
                    target $dollar target;
                    $dollar($dollar tokens)*
                }
            };
            (
                $dollar chain_visibility:vis $dollar dispatch:ident, $dollar register:ident;
                observes $dollar target:ident;
                $dollar($dollar tokens:tt)*
            ) => {
                observer_chain! {
                    $dollar chain_visibility $dollar dispatch, $dollar register;
                    target $dollar target;
                    $dollar($dollar tokens)*
                }
            };
            (
                $dollar chain_visibility:vis $dollar dispatch:ident, $dollar register:ident;
                after $dollar target:ident;
                $dollar($dollar tokens:tt)*
            ) => {
                post_chain! {
                    $dollar chain_visibility $dollar dispatch, $dollar register;
                    target $dollar target;
                    $dollar($dollar tokens)*
                }
            };
            (
                $dollar chain_visibility:vis $dollar dispatch:ident, $dollar register:ident;
                around $dollar target:ident;
                $dollar($dollar tokens:tt)*
            ) => {
                around_chain! {
                    $dollar chain_visibility $dollar dispatch, $dollar register;
                    target $dollar target;
                    $dollar($dollar tokens)*
                }
            };
        }
        $visibility use $name;
    };
}
