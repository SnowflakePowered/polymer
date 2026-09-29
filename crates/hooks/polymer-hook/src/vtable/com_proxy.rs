/// Declares a typed COM vtable hook and implements its shared storage
/// lifecycle and saved-slot accessors.
///
/// The declared hook struct is a stable, non-generic installation namespace.
/// Its generated private state stores the patched clone, saved original slots,
/// original vtable pointer, and cloned-layout marker. A slot whose return type
/// is `Option<T>` is available only when the clone's concrete vtable type
/// includes the layout named by its `vtable` attribute.
///
/// The `#[state = Type]` declaration selects state carried by each registered
/// hook object. The state type is defined outside the macro. Every other hooked method declares its
/// per-call context inside its body with `#[context] |state| { ... }`, where
/// `state` exposes those persistent fields. The macro owns registration,
/// vtable publication, chain dispatch, and the release lifecycle.
///
/// Methods accept the same phase shorthands as `chain_hooks!`:
/// `#[prefix(observer(arguments...))]` runs read-only observers before the
/// continuation, `#[infix(hook, ...)]` declares hooks that receive the
/// context by value and may call `next!`, and
/// `#[postfix(observer(arguments..., result))]` runs after continuation.
/// Prefix and postfix observers receive the context by shared reference.
/// Trivial observers can be declared inline as `#[prefix(|context| expr)]`
/// and `#[postfix(|result, context| expr)]`.
#[macro_export]
macro_rules! impl_com_proxy {
    (
        $visibility_trait:vis trait $hook_trait:ident;
        impl $hook_trait_impl:ident for $hook:ident {
            #![vtable = $storage:ty]
            $(#[state = $common_context:ty])?
            #[release $( (destroyed = $destroyed:path) )?]
            unsafe extern "system" fn $release_dispatch:ident(
                $release_this:ident: $release_this_type:ty $(,)?
            ) -> u32;
            $($methods:tt)*
        }
    ) => {
        $crate::impl_com_proxy! {
            $visibility_trait trait $hook_trait;
            impl $hook_trait_impl for $hook {
                #![vtable = $storage]
                $(#[state = $common_context])?
                #[release $( (destroyed = $destroyed) )?]
                unsafe extern "system" fn $release_dispatch(
                    $release_this: $release_this_type
                ) -> u32 { #[context] struct {} }
                $($methods)*
            }
        }
    };
    (
        $visibility_trait:vis trait $hook_trait:ident;
        impl $hook_trait_impl:ident for $hook:ident {
            #![vtable = $storage:ty]
            $(#[state = $common_context:ty])?
            #[release $( (destroyed = $destroyed:path) )?]
            unsafe extern "system" fn $release_dispatch:ident(
                $release_this:ident: $release_this_type:ty $(,)?
            ) -> u32 {
                #[context] struct {
                    $($(#[$context_attribute:meta])* $context:ident: $context_type:ty),* $(,)?
                }
            }
            $($methods:tt)*
        }
    ) => {
        $crate::impl_com_hook! {
            $visibility_trait trait $hook_trait;
            impl $hook_trait_impl for $hook {
                #![vtable = $storage]
                #[interfaces] {}
                $(#[state = $common_context])?
                #[release $( (destroyed = $destroyed) )?]
                unsafe extern "system" fn $release_dispatch(
                    $release_this: $release_this_type
                ) -> u32 {
                    #[context] struct {
                        $($(#[$context_attribute])* $context: $context_type),*
                    }
                }
                $($methods)*
            }
        }

        impl $hook {
            /// Wraps `inner` in a proxy and installs the typed hook on it.
            /// The proxy takes ownership of the caller's existing COM reference.
            $visibility_trait unsafe fn wrap<A: Vtable>(
                inner: *mut ::core::ffi::c_void,
                iid: $crate::__GUID,
                $(state: $common_context,)?
                $($context: $context_type),*
            ) -> Option<*mut ::core::ffi::c_void> {
                let proxy = unsafe { $crate::ComProxy::new(inner, iid, A::SLOTS)? };
                if unsafe {
                    Self::install_object::<A>(
                        proxy,
                        $({ let _: &$common_context = &state; state },)?
                        $($context),*
                    )
                } {
                    Some(proxy)
                } else {
                    // Installation failed before ownership could be published.
                    let _ = unsafe { $crate::ComProxy::abandon(proxy) };
                    None
                }
            }
        }
    };
}

#[macro_export]
#[doc(hidden)]
macro_rules! __impl_com_dispatches {
    (common []; $($rest:tt)*) => {
        $crate::__impl_com_dispatches!(@generate [] []; $($rest)*);
    };
    (common [$common:ty]; $($rest:tt)*) => {
        $crate::__impl_com_dispatches!(@generate
            [pub state: &'a $common,]
            [$common];
            $($rest)*
        );
    };
    (@generate $common_fields:tt $common_values:tt;
        registry $registry:ident;
        $(
            $(#[fallback = $fallback:expr])?
            [$(prefix($($prefix:tt)*))?]
            [$(infix($($infix:path),* $(,)?))?]
            [$(postfix($($postfix:tt)*))?]
            $visibility:vis unsafe extern "system" fn $dispatch:ident(
                $this:ident: $this_type:ty
                $(, $argument:ident: $argument_type:ty)* $(,)?
            ) $(-> $result:ty)?
            [$(#[context] |$state:ident| {
                $($dispatch_field:ident: $dispatch_field_type:ty = $dispatch_field_value:expr),*
                $(,)?
            })?]
        )*
    ) => { $(
        $crate::__impl_com_dispatch! {
            $common_fields $common_values;
            registry $registry;
            $(#[fallback = $fallback])?
            [$(prefix($($prefix)*))?]
            [$(infix($($infix),*))?]
            [$(postfix($($postfix)*))?]
            $visibility unsafe extern "system" fn $dispatch(
                $this: $this_type $(, $argument: $argument_type)*
            ) $(-> $result)?
            [$(#[context] |$state| {
                $($dispatch_field: $dispatch_field_type = $dispatch_field_value),*
            })?]
        }
    )* };
}

#[macro_export]
#[doc(hidden)]
macro_rules! __impl_com_dispatch {
    ([$($common_field:tt)*] [];
        $($rest:tt)*
    ) => {
        $crate::__impl_com_dispatch!(@generate [$($common_field)*] [] state __state; $($rest)*);
    };
    ([$($common_field:tt)*] [$common:ty];
        $($rest:tt)*
    ) => {
        $crate::__impl_com_dispatch!(@generate
            [$($common_field)*] [state: __state.state(),] state __state; $($rest)*
        );
    };
    (@generate [$($common_field:tt)*] [$($common_value:tt)*] state $hook_state:ident;
        registry $registry:ident;
        $(#[fallback = $fallback:expr])?
        [$(prefix($($prefix:tt)*))?]
        [$(infix($($infix:path),* $(,)?))?]
        [$(postfix($($postfix:tt)*))?]
        $visibility:vis unsafe extern "system" fn $dispatch:ident(
            $this:ident: $this_type:ty
            $(, $argument:ident: $argument_type:ty)* $(,)?
        ) $(-> $result:ty)?
        [$(#[context] |$state:ident| {
            $($dispatch_field:ident: $dispatch_field_type:ty = $dispatch_field_value:expr),*
            $(,)?
        })?]
    ) => { $crate::__paste! {
        $visibility type [<$dispatch:camel Hook>] = unsafe fn(
            $this_type,
            $($argument_type,)*
            [<$dispatch:camel Context>]<'_>,
        ) $(-> $result)?;

        $visibility struct [<$dispatch:camel Context>]<'a> {
            hooks: ::core::slice::Iter<'a, [<$dispatch:camel Hook>]>,
            terminal: &'a [<$dispatch:camel Hook>],
            base: unsafe extern "system" fn($this_type, $($argument_type),*) $(-> $result)?,
            $($common_field)*
            $($($visibility $dispatch_field: &'a $dispatch_field_type,)*)?
        }

        impl<'a> $crate::HookChain<'a, [<$dispatch:camel Hook>]>
            for [<$dispatch:camel Context>]<'a>
        {
            fn fp_next(&mut self) -> Option<&'a [<$dispatch:camel Hook>]> {
                Some(self.hooks.next().unwrap_or(self.terminal))
            }
        }

        $visibility unsafe extern "system" fn $dispatch(
            $this: $this_type,
            $($argument: $argument_type),*
        ) $(-> $result)? {
            unsafe fn terminal(
                $this: $this_type,
                $($argument: $argument_type,)*
                context: [<$dispatch:camel Context>]<'_>,
            ) $(-> $result)? {
                unsafe { (context.base)($this, $($argument),*) }
            }

            let Some(key) = $crate::__PointerKey::new(
                $this.cast::<::core::ffi::c_void>()
            ) else {
                $crate::impl_com_hook!(@missing $($fallback)?);
            };
            let Some($hook_state) = $registry
                .get(&key)
                .map(|entry| ::std::sync::Arc::clone(entry.value()))
            else {
                $crate::impl_com_hook!(@missing $($fallback)?);
            };
            let Some(base) = $hook_state.[<base_ $dispatch>]() else {
                $crate::impl_com_hook!(@missing $($fallback)?);
            };
            $($(let $state = &*$hook_state;
                let $dispatch_field = $dispatch_field_value;)*)?
            let hooks: &[[<$dispatch:camel Hook>]] =
                $crate::__chain_hooks_call!(@infix_slice_as [<$dispatch:camel Hook>];
                    $(infix($($infix),*))?
                );
            let mut remaining = hooks.iter();
            let terminal: [<$dispatch:camel Hook>] = terminal;
            let context = [<$dispatch:camel Context>] {
                hooks: remaining.clone(), terminal: &terminal, base,
                $($common_value)*
                $($($dispatch_field: &$dispatch_field,)*)?
            };
            let prefix: &[fn(
                $this_type, $($argument_type,)* &[<$dispatch:camel Context>]<'_>,
            )] = $crate::__chain_hooks_call!(@prefix_slice
                [($this, $this_type) $(($argument, $argument_type))*]
                [[<$dispatch:camel Context>]] [yes] [];
                $(prefix($($prefix)*))?
            );
            for observer in prefix {
                observer($this, $($argument,)* &context);
            }
            let result = if let Some(next) = remaining.next() {
                unsafe { next($this, $($argument,)* [<$dispatch:camel Context>] {
                    hooks: remaining, terminal: &terminal, base,
                    $($common_value)*
                    $($($dispatch_field: &$dispatch_field,)*)?
                }) }
            } else {
                unsafe { base($this, $($argument),*) }
            };
            let context = [<$dispatch:camel Context>] {
                hooks: [].iter(), terminal: &terminal, base,
                $($common_value)*
                $($($dispatch_field: &$dispatch_field,)*)?
            };
            let postfix: &[fn(
                $this_type, $($argument_type,)*
                $crate::impl_com_hook!(@result_type $($result)?),
                &[<$dispatch:camel Context>]<'_>,
            )] = $crate::__chain_hooks_call!(@postfix_slice
                [($this, $this_type) $(($argument, $argument_type))*]
                [$crate::impl_com_hook!(@result_type $($result)?)]
                [[<$dispatch:camel Context>]] [yes] [];
                $(postfix($($postfix)*))?
            );
            for observer in postfix {
                observer($this, $($argument,)* result, &context);
            }
            result
        }
    }};
}
