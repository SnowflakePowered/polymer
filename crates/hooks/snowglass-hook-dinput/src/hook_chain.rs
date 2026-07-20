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

snowglass_hook::define_chain_macro! {
    pub(crate) chain => {
        observer {
            order snowglass_hook::ChainOrder::Reverse;
            resolve with __resolve_observer_terminal;
        }
        post {
            order snowglass_hook::ChainOrder::Reverse;
            resolve with __resolve_observer_terminal;
        }
        around {
            order reverse;
            resolve with __resolve_observer_terminal;
        }
    },
    $
}
