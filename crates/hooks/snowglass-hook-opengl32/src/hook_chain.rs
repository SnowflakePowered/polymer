//! OpenGL-specific terminal resolution for shared typed hook chains.

macro_rules! resolve_terminal {
    (($($argument:ident),*); resolve with detour $detours:path, $field:ident -> $fallback:expr;) => {
        || {
            $detours
                .get()
                .map_or($fallback, |detours| unsafe { detours.$field.call($($argument),*) })
        }
    };
    (($($argument:ident),*); resolve with $terminal:path;) => {
        || unsafe { $terminal($($argument),*) }
    };
}
pub(crate) use resolve_terminal as __resolve_terminal;

macro_rules! resolve_around_terminal {
    (
        $visibility:vis $dispatch:ident, $register:ident;
        order $order:ident;
        target $target:ident;
        chain $chain:ident, context $context:ident;
        $(context fields {
            $($context_field:ident: $context_field_type:ty = $context_field_value:expr),* $(,)?
        };)?
        hook $hook:ident($($argument:ident: $argument_type:ty),* $(,)?) -> $result:ty;
        abi ($($abi_arg:ident: $abi_arg_type:ty),* $(,)?);
        export $export:ident;
        resolve with $terminal:path;
    ) => {
        snowglass_hook::define_around_chain! {
            $visibility $dispatch, $register;
            order $order;
            target $target;
            chain $chain, context $context;
            hook $hook($($argument: $argument_type),*) -> $result;
            abi ($($abi_arg: $abi_arg_type),*);
            prepare {}
            fields { $($($context_field: $context_field_type = $context_field_value),*)? }
            terminal $terminal;
            export $export;
        }
    };
}
pub(crate) use resolve_around_terminal as __resolve_around_terminal;

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
            resolve with __resolve_around_terminal;
        }
    },
    $
}
