//! DirectInput-specific terminal resolution for shared typed hook chains.

macro_rules! resolve_observer_terminal {
    (($resolved:ident $(, $argument:ident)*); resolve with $resolver:ident: $resolver_type:ty;) => {
        || unsafe { $resolved($($argument),*) }
    };
    (($($argument:ident),*); resolve with $terminal:path;) => {
        || unsafe { $terminal($($argument),*) }
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
