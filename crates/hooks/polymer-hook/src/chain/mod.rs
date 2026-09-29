mod call;
mod emit;
mod installer;
mod items;
mod terminal;

/// Declares static typed hook chains using function-shaped Rust syntax.
///
/// Each terminal is declared as `direct = path`, `installed` with a fallback,
/// or a backend-defined `dynamic(...)` source. An installed raw-hook group can
/// select its lifecycle owner with `#![detour(Type)]`, or mutually exclusive
/// `#![cfg_attr(target_os = "macos", detour(Type))]` attributes. Conditional
/// backends expand only on matching targets, including their installer state.
/// Each installed terminal supplies its own `target` and optional `continuation`.
/// A generated private registry owns transactional installation, idempotence,
/// partial cleanup, and reinstallation. Dynamic sources select
/// their callback with `#![terminal_resolver(name)]`; direct and installed-only
/// declarations need no resolver. Hook and context names are inferred from the
/// declared function name in the same way as [`function_table_hook!`]. Mark an
/// installer with `state = Type` to inject its shared state into every hook
/// context and return the state from `install`. Mark an entry with
/// `#[trampoline]` when its declared function is used as an ABI
/// detour target. `#[trampoline(abi = "...")]` overrides that entry point's ABI
/// while leaving the chain declaration as the source of its signature.
#[doc(hidden)]
#[macro_export]
macro_rules! chain_hooks {
    (
        #![order($order:ident)]
        #![cfg_attr($condition:meta, detour($backend:ty))]
        $($items:tt)*
    ) => {
        $crate::__chain_hooks_installer!(@collect_detours [$order] [[$condition] [$backend]]; $($items)*);
    };

    (
        #![terminal_resolver($resolve:ident)]
        #![order($order:ident)]
        #![detour($detour:ty)]
        #![state($state_type:ty = $state_value:expr)]
        $($items:tt)*
    ) => {
        $crate::__chain_hooks_items!(@items $order, $resolve, [$detour],
            [state: $state_type = $state_value]; $($items)*
        );
    };

    (
        #![order($order:ident)]
        #![detour($detour:ty)]
        #![state($state_type:ty = $state_value:expr)]
        $($items:tt)*
    ) => {
        $crate::__chain_hooks_items!(@items $order, __unused_terminal_resolver, [$detour],
            [state: $state_type = $state_value]; $($items)*
        );
    };

    (
        #![order($order:ident)]
        #![detour($detour:ty)]
        #[installer(state = $state:ty)]
        $installer_visibility:vis struct $installer:ident;
        $($items:tt)*
    ) => { $crate::__paste! {
        $crate::__chain_hooks_installer!(@declare_installer
            [$installer_visibility] [$installer] [state $state] [$($items)*]
        );
        impl $crate::ManagedHookInstallation for $installer {
            fn create() -> Result<Self, $crate::HookInstallError<Self>> { Self::create() }
            fn uninstall_terminals(&mut self) -> Result<(), $crate::TerminalUninstallError> {
                self.uninstall_terminals()
            }
            fn is_complete(&self) -> bool { self.is_complete() }
            fn is_empty(&self) -> bool { self.is_empty() }
        }
        #[allow(non_upper_case_globals)]
        static [<__ $installer _registry>]:
            $crate::HookInstallationRegistry<$installer> =
                $crate::HookInstallationRegistry::new();
        impl $installer {
            $installer_visibility fn install() -> Result<
                ::std::sync::Arc<$state>,
                $crate::HookRegistryError,
            > {
                [<__ $installer _registry>].install()?;
                Ok([<$installer:upper _STATE>].read()
                    .as_ref().cloned()
                    .expect("raw hook state is not installed"))
            }
            $installer_visibility fn uninstall() -> Result<(), $crate::HookRegistryError> {
                [<__ $installer _registry>].uninstall()
            }
            $installer_visibility fn with_state<T>(
                select: impl FnOnce(&$state) -> T,
            ) -> Option<T> {
                [<$installer:upper _STATE>].read()
                    .as_deref()
                    .map(select)
            }
        }
        $crate::chain_hooks! {
            #![order($order)]
            #![detour($detour)]
            #![state(::std::sync::Arc<$state> =
                [<$installer:upper _STATE>].read()
                    .as_ref().cloned()
                    .expect("raw hook state is not installed")
            )]
            $($items)*
        }
    }};

    (
        #![order($order:ident)]
        #![detour($detour:ty)]
        #[installer]
        $installer_visibility:vis struct $installer:ident;
        $($items:tt)*
    ) => { $crate::__paste! {
        $crate::__chain_hooks_installer!(@declare_installer
            [$installer_visibility] [$installer] [stateless] [$($items)*]
        );
        impl $crate::ManagedHookInstallation for $installer {
            fn create() -> Result<Self, $crate::HookInstallError<Self>> { Self::create() }
            fn uninstall_terminals(&mut self) -> Result<(), $crate::TerminalUninstallError> {
                self.uninstall_terminals()
            }
            fn is_complete(&self) -> bool { self.is_complete() }
            fn is_empty(&self) -> bool { self.is_empty() }
        }
        #[allow(non_upper_case_globals)]
        static [<__ $installer _registry>]:
            $crate::HookInstallationRegistry<$installer> =
                $crate::HookInstallationRegistry::new();
        impl $installer {
            $installer_visibility fn install() -> Result<(), $crate::HookRegistryError> {
                [<__ $installer _registry>].install()
            }
            $installer_visibility fn uninstall() -> Result<(), $crate::HookRegistryError> {
                [<__ $installer _registry>].uninstall()
            }
        }
        $crate::chain_hooks! {
            #![order($order)]
            #![detour($detour)]
            $($items)*
        }
    }};

    (
        #![terminal_resolver($resolve:ident)]
        #![order($order:ident)]
        #![detour($detour:ty)]
        #[context]
        $context_visibility:vis struct $common_context:ident {
            $($field_visibility:vis $field:ident: $field_type:ty = $field_value:expr),* $(,)?
        }
        $($items:tt)*
    ) => {
        $context_visibility struct $common_context {
            $($field_visibility $field: $field_type,)*
        }
        $crate::__chain_hooks_items!(@items $order, $resolve, [$detour],
            [common: $common_context = $common_context { $($field: $field_value,)* }];
            $($items)*
        );
    };

    (
        #![order($order:ident)]
        #![detour($detour:ty)]
        #[context]
        $context_visibility:vis struct $common_context:ident {
            $($field_visibility:vis $field:ident: $field_type:ty = $field_value:expr),* $(,)?
        }
        $($items:tt)*
    ) => {
        $context_visibility struct $common_context {
            $($field_visibility $field: $field_type,)*
        }
        $crate::__chain_hooks_items!(@items $order, __unused_terminal_resolver, [$detour],
            [common: $common_context = $common_context { $($field: $field_value,)* }];
            $($items)*
        );
    };

    (
        #![terminal_resolver($resolve:ident)]
        #![order($order:ident)]
        #![detour($detour:ty)]
        $($items:tt)*
    ) => {
        $crate::__chain_hooks_items!(@items $order, $resolve, [$detour], []; $($items)*);
    };

    (
        #![order($order:ident)]
        #![detour($detour:ty)]
        $($items:tt)*
    ) => {
        $crate::__chain_hooks_items!(@items $order, __unused_terminal_resolver, [$detour], []; $($items)*);
    };

    (
        #![terminal_resolver($resolve:ident)]
        #![order($order:ident)]
        #[context]
        $context_visibility:vis struct $common_context:ident {
            $($field_visibility:vis $field:ident: $field_type:ty = $field_value:expr),* $(,)?
        }
        $($items:tt)*
    ) => {
        $context_visibility struct $common_context {
            $($field_visibility $field: $field_type,)*
        }
        $crate::__chain_hooks_items!(@items $order, $resolve, [],
            [common: $common_context = $common_context { $($field: $field_value,)* }];
            $($items)*
        );
    };

    (
        #![order($order:ident)]
        #[context]
        $context_visibility:vis struct $common_context:ident {
            $($field_visibility:vis $field:ident: $field_type:ty = $field_value:expr),* $(,)?
        }
        $($items:tt)*
    ) => {
        $context_visibility struct $common_context {
            $($field_visibility $field: $field_type,)*
        }
        $crate::__chain_hooks_items!(@items $order, __unused_terminal_resolver, [],
            [common: $common_context = $common_context { $($field: $field_value,)* }];
            $($items)*
        );
    };

    (
        #![terminal_resolver($resolve:ident)]
        #![order($order:ident)]
        $($items:tt)*
    ) => {
        $crate::__chain_hooks_items!(@items $order, $resolve, [], []; $($items)*);
    };

    (
        #![order($order:ident)]
        $($items:tt)*
    ) => {
        $crate::__chain_hooks_items!(@items $order, __unused_terminal_resolver, [], []; $($items)*);
    };
}
