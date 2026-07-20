macro_rules! resolve_terminal {
    (($($argument:ident),*); resolve with detour $detours:path, $field:ident -> $fallback:expr;) => {
        || {
            snowglass_platform_windows::call_original(
                $detours.get().map(|detours| &detours.$field),
                $fallback,
                |detour| unsafe { (detour.trampoline())($($argument),*) },
            )
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
