macro_rules! resolve_terminal {
    (($($argument:ident),*); resolve with detour $detours:path, $field:ident -> $fallback:expr;) => {
        || {
            match $detours.get().map(|detours| &detours.$field) {
                Some(detour) if detour.is_enabled() => unsafe { detour.call($($argument),*) },
                Some(_) | None => $fallback,
            }
        }
    };
    (($($argument:ident),*); resolve with $terminal:path;) => {
        || unsafe { $terminal($($argument),*) }
    };
}
pub(crate) use resolve_terminal as __resolve_terminal;

macro_rules! chain_hooks {
    ($($tokens:tt)*) => {
        snowglass_hook::__chain_hooks! {
            resolve with __resolve_terminal;
            $($tokens)*
        }
    };
}
pub(crate) use chain_hooks;
