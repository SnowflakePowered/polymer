macro_rules! resolve_terminal {
    (($($argument:ident),*); resolve with detour $detour:path, fallback $fallback:expr;) => {
        || snowglass_platform_windows::call_original(
            $detour.get(),
            $fallback,
            |detour| unsafe { (detour.trampoline())($($argument),*) },
        )
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
