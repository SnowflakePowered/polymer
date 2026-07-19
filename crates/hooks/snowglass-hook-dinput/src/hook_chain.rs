//! DirectInput-specific terminal resolution for shared typed hook chains.

macro_rules! invoke_original_method {
    ($this:expr, $method:expr $(, $argument:expr)* $(,)?) => {{
        let method = $method;
        let slot = $this.cast::<*const usize>();
        unsafe { *slot = method.original_vtable };
        let result = unsafe { (method.function)($this $(, $argument)*) };
        unsafe { *slot = method.intercepted_vtable };
        result
    }};
}
pub(crate) use invoke_original_method as __invoke_original_method;

macro_rules! resolve_proc {
    ($runtime:expr, direct_input, $this:expr, $field:ident) => {
        $runtime.direct_input_method($this, |vtable| vtable.$field())
    };
    ($runtime:expr, device, $this:expr, $field:ident) => {
        $runtime.device_method($this, |vtable| vtable.$field())
    };
}
pub(crate) use resolve_proc as __resolve_proc;

macro_rules! resolve_observer_terminal {
    (($resolved:ident $(, $argument:ident)*); resolve with $resolver:ident: $resolver_type:ty;) => {
        || unsafe { $resolved($($argument),*) }
    };
    (($($argument:ident),*); resolve with $terminal:path;) => {
        || unsafe { $terminal($($argument),*) }
    };
    (($this:ident $(, $argument:ident)*); resolve with $domain:ident, $field:ident -> $fallback:expr;) => {
        || {
            let runtime = crate::install::runtime();
            let Some(method) = crate::hook_chain::__resolve_proc!(runtime, $domain, $this, $field) else {
                return $fallback;
            };
            crate::hook_chain::__invoke_original_method!($this, method $(, $argument)*)
        }
    };
}
pub(crate) use resolve_observer_terminal as __resolve_observer_terminal;

macro_rules! resolve_around_terminal {
    (
        $visibility:vis $dispatch:ident, $register:ident;
        order $order:ident;
        target $target:ident;
        chain $chain:ident, context $context:ident;
        $(context fields {
            $($context_field:ident: $context_field_type:ty = $context_field_value:expr),* $(,)?
        };)?
        hook $hook:ident($this:ident: $this_type:ty $(, $argument:ident: $argument_type:ty)* $(,)?) -> $result:ty;
        abi ($($abi_arg:ident: $abi_arg_type:ty),* $(,)?);
        export $export:ident;
        resolve with $domain:ident, $field:ident -> $fallback:expr;
    ) => {
        snowglass_hook::define_around_chain! {
            $visibility $dispatch, $register;
            order $order;
            target $target;
            chain $chain, context $context;
            hook $hook($this: $this_type $(, $argument: $argument_type)*) -> $result;
            abi ($($abi_arg: $abi_arg_type),*);
            prepare {}
            fields { $($($context_field: $context_field_type = $context_field_value),*)? }
            terminal |$this, $($argument,)* context: $context<'_>| {
                let Some(method) = crate::hook_chain::__resolve_proc!(context.runtime, $domain, $this, $field) else {
                    return $fallback;
                };
                crate::hook_chain::__invoke_original_method!($this, method $(, $argument)*)
            };
            export $export;
        }
    };
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
            resolve with __resolve_observer_terminal;
        }
        post {
            order snowglass_hook::ChainOrder::Reverse;
            resolve with __resolve_observer_terminal;
        }
        around {
            order reverse;
            resolve with __resolve_around_terminal;
        }
    },
    $
}
