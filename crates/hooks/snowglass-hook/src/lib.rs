//! Shared typed-hook infrastructure.

pub mod config;
mod vtable;

#[doc(hidden)]
pub use dashmap::DashMap as __DashMap;
#[doc(hidden)]
pub use paste::paste as __paste;
pub use vtable::VtableHook;

#[cfg(test)]
fn register_hooks() {}

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

/// Registration order used while visiting a hook chain.
#[derive(Clone, Copy)]
pub enum ChainOrder {
    Forward,
    Reverse,
}

/// Declares an idempotent hook-registration entry point.
#[macro_export]
macro_rules! register_hooks {
    (
        $($($register:ident)::+ -> $hook:expr;)+
    ) => {
        pub(crate) fn register_hooks() {
            static REGISTER_HOOKS: ::std::sync::Once = ::std::sync::Once::new();
            REGISTER_HOOKS.call_once(|| {
                $($($register)::+($hook);)+
            });
        }
    };
}

#[cfg(test)]
mod registration_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static REGISTRATIONS: AtomicUsize = AtomicUsize::new(0);

    fn record_registration() {
        REGISTRATIONS.fetch_add(1, Ordering::Relaxed);
    }

    fn install_registration(registration: fn()) {
        registration();
    }

    crate::register_hooks! {
        install_registration -> record_registration;
    }

    #[test]
    fn hook_registration_runs_once() {
        register_hooks();
        register_hooks();
        assert_eq!(REGISTRATIONS.load(Ordering::Relaxed), 1);
    }
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

/// Declares the data passed to observers and post hooks for one dispatch.
#[macro_export]
macro_rules! declare_chain_data_context {
    (
        $visibility:vis struct $context:ident;
        fields { $($field:ident: $field_type:ty),* $(,)? };
    ) => {
        $visibility struct $context {
            $(pub(crate) $field: $field_type,)*
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
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),* $(,)?
        ) -> $result:ty => $dispatch:path;
    ) => {
        $(#[$attribute])*
        $visibility unsafe extern $abi fn $name(
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
                chain $dollar chain:ident, context $dollar context:ident {
                    $dollar($dollar field:ident: $dollar field_type:ty = $dollar field_value:expr),* $dollar(,)?
                };
                hook $dollar hook:ident($dollar($dollar hook_arg:ident: $dollar hook_arg_type:ty),* $dollar(,)?)
                    -> $dollar result_name:ident: $dollar result:ty;
                $dollar($dollar tokens:tt)*
            ) => {
                $name! {
                    $dollar chain_visibility $dollar dispatch, $dollar register;
                    target $dollar target;
                    chain $dollar chain, context $dollar context {
                        $dollar($dollar field: $dollar field_type = $dollar field_value),*
                    };
                    hook $dollar hook($dollar($dollar hook_arg: $dollar hook_arg_type),*)
                        -> $dollar result;
                    $dollar($dollar tokens)*
                }
            };
            (
                $dollar chain_visibility:vis $dollar dispatch:ident, $dollar register:ident;
                target $dollar target:ident;
                chain $dollar chain:ident, context $dollar context:ident {
                    $dollar($dollar field:ident: $dollar field_type:ty = $dollar field_value:expr),* $dollar(,)?
                };
                hook $dollar hook:ident($dollar($dollar hook_arg:ident: $dollar hook_arg_type:ty),* $dollar(,)?)
                    -> $dollar result:ty;
                abi ($dollar($dollar abi_arg:ident: $dollar abi_arg_type:ty),* $dollar(,)?);
                $dollar($dollar resolve_tokens:tt)*
            ) => {
                type $dollar hook = unsafe fn(
                    $dollar($dollar hook_arg_type,)*
                    &$dollar context,
                );
                $crate::declare_chain_data_context! {
                    $dollar chain_visibility struct $dollar context;
                    fields { $dollar($dollar field: $dollar field_type),* };
                }
                $crate::declare_chain_storage! {
                    static $dollar chain for $dollar hook;
                    $dollar chain_visibility register $dollar register;
                }
                $dollar chain_visibility unsafe fn $dollar dispatch(
                    $dollar($dollar abi_arg: $dollar abi_arg_type),*
                ) -> $dollar result {
                    crate::register_hooks();
                    $dollar(let $dollar field = $dollar field_value;)*
                    let context = $dollar context { $dollar($dollar field,)* };
                    $crate::dispatch_observer_chain(
                        &$dollar chain,
                        $order,
                        |hook| unsafe { hook($dollar($dollar hook_arg,)* &context) },
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
                chain $dollar chain:ident, context $dollar context:ident {
                    $dollar($dollar field:ident: $dollar field_type:ty = $dollar field_value:expr),* $dollar(,)?
                };
                hook $dollar hook:ident($dollar($dollar hook_arg:ident: $dollar hook_arg_type:ty),* $dollar(,)?)
                    -> $dollar result:ty;
                resolve with $dollar($dollar resolve_tokens:tt)*
            ) => {
                $name! {
                    $dollar chain_visibility $dollar dispatch, $dollar register;
                    target $dollar target;
                    chain $dollar chain, context $dollar context {
                        $dollar($dollar field: $dollar field_type = $dollar field_value),*
                    };
                    hook $dollar hook($dollar($dollar hook_arg: $dollar hook_arg_type),*)
                        -> $dollar result;
                    abi ($dollar($dollar hook_arg: $dollar hook_arg_type),*);
                    resolve with $dollar($dollar resolve_tokens)*
                }
            };
            (
                $dollar chain_visibility:vis $dollar dispatch:ident, $dollar register:ident;
                target $dollar target:ident;
                chain $dollar chain:ident, context $dollar context:ident {
                    $dollar($dollar field:ident: $dollar field_type:ty = $dollar field_value:expr),* $dollar(,)?
                };
                hook $dollar hook:ident($dollar($dollar hook_arg:ident: $dollar hook_arg_type:ty),* $dollar(,)?);
                abi ($dollar($dollar abi_arg:ident: $dollar abi_arg_type:ty),* $dollar(,)?);
                $dollar($dollar resolve_tokens:tt)*
            ) => {
                $name! {
                    $dollar chain_visibility $dollar dispatch, $dollar register;
                    target $dollar target;
                    chain $dollar chain, context $dollar context {
                        $dollar($dollar field: $dollar field_type = $dollar field_value),*
                    };
                    hook $dollar hook($dollar($dollar hook_arg: $dollar hook_arg_type),*) -> ();
                    abi ($dollar($dollar abi_arg: $dollar abi_arg_type),*);
                    $dollar($dollar resolve_tokens)*
                }
            };
            (
                $dollar chain_visibility:vis $dollar dispatch:ident, $dollar register:ident;
                target $dollar target:ident;
                chain $dollar chain:ident, context $dollar context:ident {
                    $dollar($dollar field:ident: $dollar field_type:ty = $dollar field_value:expr),* $dollar(,)?
                };
                hook $dollar hook:ident($dollar($dollar hook_arg:ident: $dollar hook_arg_type:ty),* $dollar(,)?);
                resolve with $dollar($dollar resolve_tokens:tt)*
            ) => {
                $name! {
                    $dollar chain_visibility $dollar dispatch, $dollar register;
                    target $dollar target;
                    chain $dollar chain, context $dollar context {
                        $dollar($dollar field: $dollar field_type = $dollar field_value),*
                    };
                    hook $dollar hook($dollar($dollar hook_arg: $dollar hook_arg_type),*);
                    abi ($dollar($dollar hook_arg: $dollar hook_arg_type),*);
                    resolve with $dollar($dollar resolve_tokens)*
                }
            };
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
                chain $dollar chain:ident, context $dollar context:ident {
                    $dollar($dollar field:ident: $dollar field_type:ty = $dollar field_value:expr),* $dollar(,)?
                };
                hook $dollar hook:ident($dollar($dollar hook_arg:ident: $dollar hook_arg_type:ty),* $dollar(,)?)
                    -> $dollar result_argument:ident: $dollar result:ty;
                abi ($dollar($dollar abi_arg:ident: $dollar abi_arg_type:ty),* $dollar(,)?);
                $dollar($dollar resolve_tokens:tt)*
            ) => {
                type $dollar hook = unsafe fn(
                    $dollar($dollar hook_arg_type,)*
                    $dollar result,
                    &$dollar context,
                );
                $crate::declare_chain_data_context! {
                    $dollar chain_visibility struct $dollar context;
                    fields { $dollar($dollar field: $dollar field_type),* };
                }
                $crate::declare_chain_storage! {
                    static $dollar chain for $dollar hook;
                    $dollar chain_visibility register $dollar register;
                }
                $dollar chain_visibility unsafe fn $dollar dispatch(
                    $dollar($dollar abi_arg: $dollar abi_arg_type),*
                ) -> $dollar result {
                    crate::register_hooks();
                    $dollar(let $dollar field = $dollar field_value;)*
                    let context = $dollar context { $dollar($dollar field,)* };
                    $crate::dispatch_post_chain(
                        &$dollar chain,
                        $order,
                        $resolve! {
                            ($dollar($dollar abi_arg),*);
                            $dollar($dollar resolve_tokens)*
                        },
                        |hook, $dollar result_argument| unsafe {
                            hook(
                                $dollar($dollar hook_arg,)*
                                $dollar result_argument,
                                &context,
                            )
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
                chain $dollar chain:ident, context $dollar context:ident {
                    $dollar($dollar field:ident: $dollar field_type:ty = $dollar field_value:expr),* $dollar(,)?
                };
                hook $dollar hook:ident($dollar($dollar hook_arg:ident: $dollar hook_arg_type:ty),* $dollar(,)?)
                    -> $dollar result_argument:ident: $dollar result:ty;
                resolve with $dollar($dollar resolve_tokens:tt)*
            ) => {
                $name! {
                    $dollar chain_visibility $dollar dispatch, $dollar register;
                    target $dollar target;
                    chain $dollar chain, context $dollar context {
                        $dollar($dollar field: $dollar field_type = $dollar field_value),*
                    };
                    hook $dollar hook($dollar($dollar hook_arg: $dollar hook_arg_type),*)
                        -> $dollar result_argument: $dollar result;
                    abi ($dollar($dollar hook_arg: $dollar hook_arg_type),*);
                    resolve with $dollar($dollar resolve_tokens)*
                }
            };
            (
                $dollar chain_visibility:vis $dollar dispatch:ident, $dollar register:ident;
                target $dollar target:ident;
                chain $dollar chain:ident, context $dollar context:ident {
                    $dollar($dollar field:ident: $dollar field_type:ty = $dollar field_value:expr),* $dollar(,)?
                };
                hook $dollar hook:ident($dollar($dollar hook_arg:ident: $dollar hook_arg_type:ty),* $dollar(,)?)
                    -> $dollar result:ty;
                $dollar($dollar tokens:tt)*
            ) => {
                $name! {
                    $dollar chain_visibility $dollar dispatch, $dollar register;
                    target $dollar target;
                    chain $dollar chain, context $dollar context {
                        $dollar($dollar field: $dollar field_type = $dollar field_value),*
                    };
                    hook $dollar hook($dollar($dollar hook_arg: $dollar hook_arg_type),*)
                        -> result: $dollar result;
                    $dollar($dollar tokens)*
                }
            };
            (
                $dollar chain_visibility:vis $dollar dispatch:ident, $dollar register:ident;
                target $dollar target:ident;
                chain $dollar chain:ident, context $dollar context:ident {
                    $dollar($dollar field:ident: $dollar field_type:ty = $dollar field_value:expr),* $dollar(,)?
                };
                hook $dollar hook:ident($dollar($dollar hook_arg:ident: $dollar hook_arg_type:ty),* $dollar(,)?);
                abi ($dollar($dollar abi_arg:ident: $dollar abi_arg_type:ty),* $dollar(,)?);
                $dollar($dollar resolve_tokens:tt)*
            ) => {
                type $dollar hook = unsafe fn(
                    $dollar($dollar hook_arg_type,)*
                    &$dollar context,
                );
                $crate::declare_chain_data_context! {
                    $dollar chain_visibility struct $dollar context;
                    fields { $dollar($dollar field: $dollar field_type),* };
                }
                $crate::declare_chain_storage! {
                    static $dollar chain for $dollar hook;
                    $dollar chain_visibility register $dollar register;
                }
                $dollar chain_visibility unsafe fn $dollar dispatch(
                    $dollar($dollar abi_arg: $dollar abi_arg_type),*
                ) {
                    crate::register_hooks();
                    $dollar(let $dollar field = $dollar field_value;)*
                    let context = $dollar context { $dollar($dollar field,)* };
                    $crate::dispatch_post_chain(
                        &$dollar chain,
                        $order,
                        $resolve! {
                            ($dollar($dollar abi_arg),*);
                            $dollar($dollar resolve_tokens)*
                        },
                        |hook, ()| unsafe {
                            hook($dollar($dollar hook_arg,)* &context)
                        },
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
                chain $dollar chain:ident, context $dollar context:ident {
                    $dollar($dollar field:ident: $dollar field_type:ty = $dollar field_value:expr),* $dollar(,)?
                };
                hook $dollar hook:ident($dollar($dollar hook_arg:ident: $dollar hook_arg_type:ty),* $dollar(,)?);
                resolve with $dollar($dollar resolve_tokens:tt)*
            ) => {
                $name! {
                    $dollar chain_visibility $dollar dispatch, $dollar register;
                    target $dollar target;
                    chain $dollar chain, context $dollar context {
                        $dollar($dollar field: $dollar field_type = $dollar field_value),*
                    };
                    hook $dollar hook($dollar($dollar hook_arg: $dollar hook_arg_type),*);
                    abi ($dollar($dollar hook_arg: $dollar hook_arg_type),*);
                    resolve with $dollar($dollar resolve_tokens)*
                }
            };
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
                chain $dollar chain:ident, context $dollar context:ident {
                    $dollar($dollar field:ident: $dollar field_type:ty = $dollar field_value:expr),* $dollar(,)?
                };
                hook $dollar hook:ident($dollar($dollar hook_arg:ident: $dollar hook_arg_type:ty),* $dollar(,)?)
                    -> $dollar result_name:ident: $dollar result:ty;
                $dollar($dollar tokens:tt)*
            ) => {
                $name! {
                    $dollar chain_visibility $dollar dispatch, $dollar register;
                    target $dollar target;
                    chain $dollar chain, context $dollar context {
                        $dollar($dollar field: $dollar field_type = $dollar field_value),*
                    };
                    hook $dollar hook($dollar($dollar hook_arg: $dollar hook_arg_type),*)
                        -> $dollar result;
                    $dollar($dollar tokens)*
                }
            };
            (
                $dollar chain_visibility:vis $dollar dispatch:ident, $dollar register:ident;
                target $dollar target:ident;
                chain $dollar chain:ident, context $dollar context:ident {
                    $dollar($dollar field:ident: $dollar field_type:ty = $dollar field_value:expr),* $dollar(,)?
                };
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
                    context fields {
                        $dollar($dollar field: $dollar field_type = $dollar field_value),*
                    };
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
                chain $dollar chain:ident, context $dollar context:ident {
                    $dollar($dollar field:ident: $dollar field_type:ty = $dollar field_value:expr),* $dollar(,)?
                };
                hook $dollar hook:ident($dollar($dollar hook_arg:ident: $dollar hook_arg_type:ty),* $dollar(,)?)
                    -> $dollar result:ty;
                resolve with $dollar($dollar resolve_tokens:tt)*
            ) => {
                $name! {
                    $dollar chain_visibility $dollar dispatch, $dollar register;
                    target $dollar target;
                    chain $dollar chain, context $dollar context {
                        $dollar($dollar field: $dollar field_type = $dollar field_value),*
                    };
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
/// A chain may replace `context Context;` with
/// `context Context { field: Type = value, ... };`. Observer and post hooks
/// receive the generated context by shared reference as their final argument;
/// around hooks receive the fields on their continuation context.
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

#[cfg(test)]
#[allow(unused_imports)]
mod context_field_tests {
    use crate::HookChain;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static OBSERVED: AtomicUsize = AtomicUsize::new(0);
    static POSTED: AtomicUsize = AtomicUsize::new(0);

    unsafe fn terminal(value: usize) -> usize {
        value * 2
    }

    macro_rules! resolve_terminal {
        (($($argument:ident),*); resolve with terminal;) => {
            || unsafe { terminal($($argument),*) }
        };
    }
    use resolve_terminal as __resolve_terminal;

    macro_rules! resolve_around_terminal {
        (
            $visibility:vis $dispatch:ident, $register:ident;
            order $order:ident;
            target $target:ident;
            chain $chain:ident, context $context:ident;
            context fields {
                $($field:ident: $field_type:ty = $field_value:expr),* $(,)?
            };
            hook $hook:ident($($argument:ident: $argument_type:ty),* $(,)?) -> $result:ty;
            abi ($($abi_arg:ident: $abi_arg_type:ty),* $(,)?);
            export $export:ident;
            resolve with terminal;
        ) => {
            crate::define_around_chain! {
                $visibility $dispatch, $register;
                order $order;
                target $target;
                chain $chain, context $context;
                hook $hook($($argument: $argument_type),*) -> $result;
                abi ($($abi_arg: $abi_arg_type),*);
                prepare {}
                fields { $($field: $field_type = $field_value),* }
                terminal |$($argument,)* _context: $context<'_>| unsafe {
                    terminal($($argument),*)
                };
                export $export;
            }
        };
    }
    use resolve_around_terminal as __resolve_around_terminal;

    crate::define_chain_macro! {
        chain => {
            observer {
                order crate::ChainOrder::Forward;
                resolve with __resolve_terminal;
            }
            post {
                order crate::ChainOrder::Forward;
                resolve with __resolve_terminal;
            }
            around {
                order forward;
                resolve with __resolve_around_terminal;
            }
        },
        $
    }

    chain! {
        dispatch_observe, register_observe;
        observes observe_target;
        chain OBSERVE_CHAIN, context ObserveContext {
            offset: usize = 2,
        };
        hook ObserveHook(value: usize) -> usize;
        resolve with terminal;
    }

    chain! {
        dispatch_post, register_post;
        after post_target;
        chain POST_CHAIN, context PostContext {
            offset: usize = 3,
        };
        hook PostHook() -> result: usize;
        abi (value: usize);
        resolve with terminal;
    }

    chain! {
        dispatch_around, register_around;
        around around_target;
        chain AROUND_CHAIN, context AroundContext {
            offset: usize = 4,
        };
        hook AroundHook(value: usize) -> usize;
        resolve with terminal;
    }

    unsafe fn observe(value: usize, context: &ObserveContext) {
        OBSERVED.store(value + context.offset, Ordering::Relaxed);
    }

    unsafe fn post(result: usize, context: &PostContext) {
        POSTED.store(result + context.offset, Ordering::Relaxed);
    }

    unsafe fn around(value: usize, mut next: AroundContext<'_>) -> usize {
        let offset = next.offset;
        let continuation = *next.fp_next().expect("around chains have a terminal");
        unsafe { continuation(value + offset, next) }
    }

    #[test]
    fn chain_context_fields_reach_each_hook_mode() {
        register_observe(observe);
        register_post(post);
        register_around(around);

        assert_eq!(unsafe { dispatch_observe(5) }, 10);
        assert_eq!(OBSERVED.load(Ordering::Relaxed), 7);
        assert_eq!(unsafe { dispatch_post(5) }, 10);
        assert_eq!(POSTED.load(Ordering::Relaxed), 13);
        assert_eq!(unsafe { dispatch_around(5) }, 18);
    }
}
