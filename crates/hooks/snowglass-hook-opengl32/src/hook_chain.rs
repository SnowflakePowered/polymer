//! OpenGL-specific terminal resolution for shared typed hook chains.

macro_rules! resolve_terminal {
    (($($argument:ident),*); resolve with detour $detours:path, $field:ident -> $fallback:expr;) => {
        || {
            $detours
                .get()
                .map_or($fallback, |detours| unsafe {
                    (detours.$field.trampoline())($($argument),*)
                })
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
