//! Typed hook chains with argument projection.
//!
//! An exported Vulkan command can carry more ABI arguments than an observer
//! needs. The chain owns continuation through every observer; handlers only
//! receive the arguments declared when they are registered.

macro_rules! resolve_proc {
    (command_buffer, $handle:expr, $name:expr, $ty:ty) => {{
        crate::layer::command_dispatch_data($handle).and_then(|(gdpa, device)| unsafe {
            crate::terminal::device_proc::<$ty>(gdpa, device, $name)
        })
    }};
    (device, $handle:expr, $name:expr, $ty:ty) => {{
        crate::layer::device_gdpa($handle)
            .and_then(|gdpa| unsafe { crate::terminal::device_proc::<$ty>(gdpa, $handle, $name) })
    }};
    (instance, $handle:expr, $name:expr, $ty:ty) => {{
        crate::layer::instance_gipa($handle)
            .and_then(|gipa| unsafe { crate::terminal::instance_proc::<$ty>(gipa, $handle, $name) })
    }};
    (queue, $handle:expr, $name:expr, $ty:ty) => {{
        crate::layer::queue_dispatch_data($handle).and_then(|(gdpa, device)| unsafe {
            crate::terminal::device_proc::<$ty>(gdpa, device, $name)
        })
    }};
}
pub(crate) use resolve_proc;

macro_rules! resolve_observer_terminal {
    (($($abi_arg:ident),*); resolve with $resolve_arg:ident, $name:literal: $next_module:ident::$next_ty:ident;) => {
        || {
            let resolved: Option<$next_module::$next_ty> =
                crate::hook_chain::resolve_proc!($resolve_arg, $resolve_arg, $name, $next_module::$next_ty);
            if let Some(next) = resolved {
                unsafe { next($($abi_arg),*) };
            }
        }
    };
    (($($abi_arg:ident),*); resolve with $resolve_arg:ident, $name:literal: $next_module:ident::$next_ty:ident -> $fallback:expr;) => {
        || {
            let resolved: Option<$next_module::$next_ty> =
                crate::hook_chain::resolve_proc!($resolve_arg, $resolve_arg, $name, $next_module::$next_ty);
            resolved.map_or($fallback, |next| unsafe { next($($abi_arg),*) })
        }
    };
    (($($abi_arg:ident),*); resolve with $terminal:path;) => {
        || unsafe { $terminal($($abi_arg),*) }
    };
}
pub(crate) use resolve_observer_terminal as __resolve_observer_terminal;

macro_rules! chain_hooks {
    ($($tokens:tt)*) => {
        snowglass_hook::__chain_hooks! {
            resolve with __resolve_observer_terminal;
            $($tokens)*
        }
    };
}
pub(crate) use chain_hooks;
