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

snowglass_hook::define_chain_macro! {
    pub(crate) chain => {
        observer {
            order snowglass_hook::ChainOrder::Reverse;
            resolve with __resolve_terminal;
        }
        post {
            order snowglass_hook::ChainOrder::Reverse;
            resolve with __resolve_terminal;
        }
        around {
            order reverse;
            resolve with __resolve_terminal;
        }
    },
    $
}
