macro_rules! resolve_proc {
    (swapchain, $this:expr, $field:ident) => {
        crate::runtime::swapchain_method($this, |vtable| vtable.$field())
    };
    (factory, $this:expr, $field:ident) => {
        crate::runtime::factory_method($this, |vtable| vtable.$field())
    };
    (query, $this:expr, $field:ident) => {
        crate::runtime::query_method($this)
    };
}
pub(crate) use resolve_proc as __resolve_proc;

macro_rules! resolve_terminal {
    (($($argument:ident),*); resolve with detour $detours:path, $field:ident -> $fallback:expr;) => {
        || {
            snowglass_platform_windows::call_original(
                $detours.get().map(|detours| &detours.$field),
                $fallback,
                |detour| unsafe { detour.call($($argument),*) },
            )
        }
    };
    (($this:ident $(, $argument:ident)*); resolve with swapchain, $field:ident -> $fallback:expr;) => {
        || {
            let Some(function) = crate::hook_chain::__resolve_proc!(swapchain, $this, $field) else {
                return $fallback;
            };
            unsafe { function($this $(, $argument)*) }
        }
    };
    (($this:ident $(, $argument:ident)*); resolve with factory, $field:ident -> $fallback:expr;) => {
        || {
            let Some(function) = crate::hook_chain::__resolve_proc!(factory, $this, $field) else {
                return $fallback;
            };
            unsafe { function($this $(, $argument)*) }
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
        hook $hook:ident($this:ident: $this_type:ty $(, $argument:ident: $argument_type:ty)* $(,)?) -> $result:ty;
        abi ($($abi_arg:ident: $abi_arg_type:ty),* $(,)?);
        export $export:ident;
        resolve with query, $field:ident -> $fallback:expr;
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
            terminal |$this, $($argument,)* _context: $context<'_>| unsafe {
                let Some(function) = crate::hook_chain::__resolve_proc!(query, $this, $field)
                else {
                    return $fallback;
                };
                function($this $(, $argument)*)
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
        hook $hook:ident($this:ident: $this_type:ty $(, $argument:ident: $argument_type:ty)* $(,)?) -> $result:ty;
        abi ($($abi_arg:ident: $abi_arg_type:ty),* $(,)?);
        export $export:ident;
        resolve with swapchain, $field:ident -> $fallback:expr;
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
            terminal |$this, $($argument,)* _context: $context<'_>| unsafe {
                let Some(function) = crate::hook_chain::__resolve_proc!(swapchain, $this, $field)
                else {
                    return $fallback;
                };
                function($this $(, $argument)*)
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
        hook $hook:ident($this:ident: $this_type:ty $(, $argument:ident: $argument_type:ty)* $(,)?) -> $result:ty;
        abi ($($abi_arg:ident: $abi_arg_type:ty),* $(,)?);
        export $export:ident;
        resolve with swapchain1, $field:ident -> $fallback:expr;
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
            terminal |$this, $($argument,)* _context: $context<'_>| unsafe {
                let Some(function) = crate::runtime::swapchain_method(
                    $this,
                    |vtable| vtable.$field(),
                ).flatten() else {
                    return $fallback;
                };
                function($this $(, $argument)*)
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
