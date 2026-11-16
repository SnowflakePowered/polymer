#[macro_export]
macro_rules! impl_com_hook {
    (
        $visibility_trait:vis trait $hook_trait:ident;
        impl $hook_trait_impl:ident for $hook:ident {
            #![vtable = $storage:ty]
            #[interfaces] {
                $($interface:path),* $(,)?
            }
            $(#[state = $common_context:ty])?
            #[release $( (destroyed = $destroyed:path) )?]
            unsafe extern "system" fn $release_dispatch:ident(
                $release_this:ident: $release_this_type:ty $(,)?
            ) -> u32;
            $($methods:tt)*
        }
    ) => {
        $crate::impl_com_hook! {
            $visibility_trait trait $hook_trait;
            impl $hook_trait_impl for $hook {
                #![vtable = $storage]
                #[interfaces] {
                    $($interface),*
                }
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
            #[interfaces] {
                $($interface:path),* $(,)?
            }
            $(#[state = $common_context:ty])?

            #[release $( (destroyed = $destroyed:path) )?]
            unsafe extern "system" fn $release_dispatch:ident(
                $release_this:ident: $release_this_type:ty $(,)?
            ) -> u32 {
                #[context] struct {
                    $($(#[$context_attribute:meta])* $context:ident: $context_type:ty),* $(,)?
                }
            }

            $(
                #[base = $vtable_interface:ident::$base:ident]
                $(#[fallback = $fallback:expr])?
                $(#[prefix($($prefix:tt)*)])?
                $(#[infix($($infix:path),* $(,)? )])?
                $(#[postfix($($postfix:tt)*)])?
                unsafe extern "system" fn $dispatch:ident(
                    $this:ident: $this_type:ty
                    $(, $argument:ident: $argument_type:ty)* $(,)?
                ) $(-> $result:ty)? $( {
                    #[context] |$state:ident| {
                        $($dispatch_field:ident: $dispatch_field_type:ty = $dispatch_field_value:expr),*
                        $(,)?
                    }
                } )? $(;)?
            )*
        }
    ) => { $crate::__paste! {
        const _: () = {
            fn assert_same_trait<T: ?Sized>() {}
            let _ = assert_same_trait::<dyn $hook_trait_impl>;
        };

        $visibility_trait trait $hook_trait: $crate::VtableHook {
            $(fn state(&self) -> &$common_context;)?
            $(fn $context(&self) -> &$context_type;)*
            fn [<base_ $release_dispatch>](&self) -> unsafe extern "system" fn($release_this_type) -> u32;
            $(fn [<base_ $dispatch>](&self) -> Option<unsafe extern "system" fn(
                $this_type,
                $($argument_type),*
            ) $(-> $result)?>;)*
        }

        #[allow(non_upper_case_globals)]
        static [<__ $release_dispatch _registry>]: ::std::sync::LazyLock<
            $crate::__DashMap<$crate::__PointerKey, ::std::sync::Arc<dyn $hook_trait>>
        > = ::std::sync::LazyLock::new(Default::default);

        $visibility_trait struct $hook;

        struct [<$hook State>]<A: Vtable> {
            patched: Box<$storage>,
            saved: $storage,
            original: ::core::ptr::NonNull<::core::ffi::c_void>,
            vtable: ::core::marker::PhantomData<A>,
            $(state: $common_context,)?
            $($(#[$context_attribute])* $context: $context_type,)*
        }

        unsafe impl<A: Vtable> Send for [<$hook State>]<A> {}
        unsafe impl<A: Vtable> Sync for [<$hook State>]<A> {}

        impl $hook {
            /// Returns whether `object` currently publishes this hook's vtable.
            ///
            /// # Safety
            /// `object` must be null or point to a live COM object.
            $visibility_trait unsafe fn is_installed(
                object: *mut ::core::ffi::c_void,
            ) -> bool {
                if object.is_null() {
                    return false;
                }
                let key = $crate::__PointerKey::new(object)
                    .expect("COM object was validated as non-null");
                [<__ $release_dispatch _registry>]
                    .get(&key)
                    .is_some_and(|state| unsafe {
                        *object.cast::<*const ::core::ffi::c_void>() == state.patched_vtable()
                    })
            }

            $($visibility_trait fn with_state<T>(
                object: *mut ::core::ffi::c_void,
                select: impl FnOnce(&$common_context) -> T,
            ) -> Option<T> {
                let key = $crate::__PointerKey::new(object)?;
                [<__ $release_dispatch _registry>]
                    .get(&key)
                    .map(|state| select(state.state()))
            })?

            $visibility_trait unsafe fn install_widest(
                object: *mut ::core::ffi::c_void,
                $(state: $common_context,)?
                $($context: $context_type),*
            ) -> bool {
                let Some(unknown) = (unsafe {
                    <::windows::core::IUnknown as ::windows::core::Interface>::from_raw_borrowed(
                        &object
                    )
                }) else { return false; };
                $crate::impl_com_hook!(@install_widest unknown;
                    [$(state: $common_context)?]
                    [$($context: $context_type),*];
                    $($interface),*
                )
            }

            $visibility_trait unsafe fn install_all_interfaces(
                object: *mut ::core::ffi::c_void,
                $(state: $common_context,)?
                $($context: $context_type),*
            ) -> bool {
                let Some(unknown) = (unsafe {
                    <::windows::core::IUnknown as ::windows::core::Interface>::from_raw_borrowed(
                        &object
                    )
                }) else { return false; };
                let mut installed = false;
                let mut pointers: ::std::vec::Vec<
                    ::core::ptr::NonNull<::core::ffi::c_void>
                > = ::std::vec::Vec::new();
                $crate::impl_com_hook!(@install_all unknown, installed, pointers;
                    [$(state: $common_context)?]
                    [$($context: $context_type),*];
                    $($interface),*
                );
                installed
            }

            $visibility_trait unsafe fn install_interface(
                object: *mut ::core::ffi::c_void,
                interface_id: &$crate::__GUID,
                $(state: $common_context,)?
                $($context: $context_type),*
            ) -> bool {
                $crate::impl_com_hook!(@install_interface object, interface_id;
                    [$(state: $common_context)?]
                    [$($context: $context_type),*];
                    $($interface),*
                )
            }

            /// Clones, registers, and publishes a patched vtable for `object`.
            ///
            /// # Safety
            /// `object` must be null or point to a live COM object whose
            /// vtable spans at least `A::SLOTS` entries.
            $visibility_trait unsafe fn install_object<A: Vtable>(
                object: *mut ::core::ffi::c_void,
                $(state: $common_context,)?
                $($context: $context_type),*
            ) -> bool {
                if object.is_null() {
                    return false;
                }
                let key = $crate::__PointerKey::new(object)
                    .expect("COM object was validated as non-null");
                let object_vtable = object.cast::<*const ::core::ffi::c_void>();
                let current = unsafe { *object_vtable };
                let original_pointer = if let Some(existing) =
                    [<__ $release_dispatch _registry>].get(&key)
                {
                    if current == existing.patched_vtable() {
                        if A::supported_by(existing.vtable_type()) {
                            return true;
                        }
                        existing.original_vtable()
                    } else {
                        current
                    }
                } else {
                    current
                };
                unsafe {
                    Self::install_object_from::<A>(
                        object,
                        original_pointer,
                        $({ let _: &$common_context = &state; state },)?
                        $($context),*
                    )
                }
            }

            /// Installs using an explicitly selected original vtable, allowing
            /// callers to upgrade an already-hooked object to a wider layout.
            ///
            /// # Safety
            /// `object` must identify a live COM object and `original_pointer`
            /// must span at least `A::SLOTS` entries.
            $visibility_trait unsafe fn install_object_from<A: Vtable>(
                object: *mut ::core::ffi::c_void,
                original_pointer: *const ::core::ffi::c_void,
                $(state: $common_context,)?
                $($context: $context_type),*
            ) -> bool {
                if object.is_null() {
                    return false;
                }
                let object_vtable = object.cast::<*const ::core::ffi::c_void>();
                let Some(original) = ::core::ptr::NonNull::new(
                    original_pointer.cast_mut().cast::<::core::ffi::c_void>()
                ) else {
                    return false;
                };
                let saved = unsafe { <$storage>::read(original, A::SLOTS) };
                let mut patched = Box::new(unsafe { <$storage>::read(original, A::SLOTS) });
                unsafe {
                    let unknown = patched.as_mut_ptr::<::windows::core::IUnknown_Vtbl>();
                    #[allow(clippy::useless_transmute)]
                    let release = ::core::mem::transmute(
                        $release_dispatch as unsafe extern "system" fn($release_this_type) -> u32
                    );
                    (*unknown).Release = release;
                    $(if A::supports(::core::any::TypeId::of::<$vtable_interface>()) {
                        let table = patched.as_mut_ptr::<<$vtable_interface as ::windows::core::Interface>::Vtable>();
                        (*table).$base = ::core::mem::transmute(
                            $dispatch as unsafe extern "system" fn(
                                $this_type,
                                $($argument_type),*
                            ) $(-> $result)?
                        );
                    })*
                }
                let state: ::std::sync::Arc<dyn $hook_trait> =
                    ::std::sync::Arc::new([<$hook State>]::<A> {
                    patched,
                    saved,
                    original,
                    vtable: ::core::marker::PhantomData,
                    $(state: { let _: &$common_context = &state; state },)?
                    $($context,)*
                });
                let patched_vtable = state.patched_vtable();
                let key = $crate::__PointerKey::new(object)
                    .expect("COM object was validated as non-null");
                [<__ $release_dispatch _registry>].insert(key, state);
                unsafe { *object_vtable = patched_vtable };
                true
            }
        }

        impl<A: Vtable> $crate::VtableHook for [<$hook State>]<A> {
            fn slots(&self) -> usize { A::SLOTS }
            fn vtable_type(&self) -> ::core::any::TypeId { ::core::any::TypeId::of::<A>() }
            fn patched_vtable(&self) -> *const ::core::ffi::c_void {
                ::core::ptr::from_ref(self.patched.as_ref()).cast()
            }
            fn original_vtable(&self) -> *const ::core::ffi::c_void {
                self.original.as_ptr().cast_const()
            }
        }

        impl<A: Vtable> $hook_trait for [<$hook State>]<A> {
            $(fn state(&self) -> &$common_context { &self.state })?
            $(fn $context(&self) -> &$context_type { &self.$context })*
            #[allow(clippy::useless_transmute)]
            fn [<base_ $release_dispatch>](&self) -> unsafe extern "system" fn($release_this_type) -> u32 {
                unsafe {
                    ::core::mem::transmute(
                        (*::core::ptr::from_ref(&self.saved)
                            .cast::<::windows::core::IUnknown_Vtbl>()).Release
                    )
                }
            }
            $(fn [<base_ $dispatch>](&self) -> Option<unsafe extern "system" fn(
                $this_type,
                $($argument_type),*
            ) $(-> $result)?> {
                A::supports(::core::any::TypeId::of::<$vtable_interface>()).then(|| unsafe {
                    ::core::mem::transmute(
                        (*::core::ptr::from_ref(&self.saved).cast::<<$vtable_interface as ::windows::core::Interface>::Vtable>()).$base
                    )
                })
            })*
        }

        $visibility_trait unsafe extern "system" fn $release_dispatch(
            $release_this: $release_this_type,
        ) -> u32 {
            let Some(key) = $crate::__PointerKey::new(
                $release_this.cast::<::core::ffi::c_void>()
            ) else {
                return 0;
            };
            let Some(state) = [<__ $release_dispatch _registry>]
                .get(&key)
                .map(|entry| ::std::sync::Arc::clone(entry.value()))
            else {
                return 0;
            };
            let base = state.[<base_ $release_dispatch>]();
            let original_vtable = state.original_vtable();
            let patched_vtable = state.patched_vtable();
            let object_vtable = $release_this.cast::<*const ::core::ffi::c_void>();
            unsafe { *object_vtable = original_vtable };
            let remaining = unsafe { base($release_this) };
            if remaining == 0 {
                [<__ $release_dispatch _registry>]
                    .remove_if(&key, |_, current| current.patched_vtable() == patched_vtable);
                $(unsafe { $destroyed($release_this, &*state) };)?
            } else if [<__ $release_dispatch _registry>]
                .get(&key)
                .is_some_and(|current| current.patched_vtable() == patched_vtable)
            {
                unsafe { *object_vtable = patched_vtable };
            }
            remaining
        }

        $crate::__impl_com_dispatches! {
            common [$($common_context)?];
            registry [<__ $release_dispatch _registry>];
            $(
                $(#[fallback = $fallback])?
                [$(prefix($($prefix)*))?]
                [$(infix($($infix),*))?]
                [$(postfix($($postfix)*))?]
                $visibility_trait unsafe extern "system" fn $dispatch(
                    $this: $this_type $(, $argument: $argument_type)*
                ) $(-> $result)?
                [$(#[context] |$state| {
                    $($dispatch_field: $dispatch_field_type = $dispatch_field_value),*
                })?]
            )*
        }
    }};

    (@result_type $result:ty) => { $result };
    (@result_type) => { () };
    (@missing $fallback:expr) => { return $fallback };
    (@missing) => { return };

    (
        impl $hook:ident {
            #![vtable = $storage:ty]
            slots $slots:expr;
            $(context { $($(#[$context_attribute:meta])* $context:ident: $context_type:ty),* $(,)? };)?
            patch |$patched:ident| $patch:block
            $($methods:tt)*
        }
    ) => {
        pub(crate) struct $hook {
            /// Hook-patched clone installed as the object's vtable pointer.
            patched: Box<$storage>,
            /// Verbatim copy of the original slots for downstream continuation.
            saved: $storage,
            /// The object's original vtable pointer, restored across releases.
            original: ::core::ptr::NonNull<::core::ffi::c_void>,
            $($(
                $(#[$context_attribute])*
                $context: $context_type,
            )*)?
        }

        // SAFETY: the original pointer identifies an immutable vtable owned
        // by the hooked module, and the remaining fields contain copied
        // vtable data and any context fields, which must themselves be
        // `Send + Sync`.
        unsafe impl Send for $hook {}
        unsafe impl Sync for $hook {}

        impl $hook {
            /// Clones an original COM vtable and applies this hook's patches.
            ///
            /// # Safety
            ///
            /// `original` must be null or point to a vtable spanning at least
            /// the configured number of slots.
            pub(crate) unsafe fn clone_from(
                original: *const ::core::ffi::c_void
                $($(, $context: $context_type)*)?
            ) -> Option<Self> {
                let original = ::core::ptr::NonNull::new(
                    original.cast_mut().cast::<::core::ffi::c_void>()
                )?;
                let saved = unsafe { <$storage>::read(original, $slots) };
                let mut patched = Box::new(unsafe { <$storage>::read(original, $slots) });
                let $patched = patched.as_mut();
                $patch
                Some(Self {
                    patched,
                    saved,
                    original,
                    $($($context,)*)?
                })
            }

            $($(
                #[allow(dead_code)]
                pub(crate) fn $context(&self) -> &$context_type {
                    &self.$context
                }
            )*)?

            /// The vtable pointer that intercepts the hooked object.
            pub(crate) fn patched_vtable(&self) -> *const ::core::ffi::c_void {
                ::core::ptr::from_ref(self.patched.as_ref()).cast()
            }

            /// The object's original vtable pointer.
            pub(crate) fn original_vtable(&self) -> *const ::core::ffi::c_void {
                self.original.as_ptr().cast_const()
            }

            $crate::impl_com_hook!(@inherent_methods $($methods)*);
        }
    };

    (
        impl $hook_trait:ident for $hook:ident {
            #![vtable = $storage:ty]
            $(context { $($(#[$context_attribute:meta])* $context:ident: $context_type:ty),* $(,)? };)?
            patch |$patched:ident| $patch:block
            $($methods:tt)*
        }
    ) => {
        pub(crate) struct $hook<A: Vtable> {
            /// Hook-patched clone installed as the object's vtable pointer.
            patched: Box<$storage>,
            /// Verbatim copy of the original slots for downstream continuation.
            saved: $storage,
            /// The object's original vtable pointer, restored across releases.
            original: ::core::ptr::NonNull<::core::ffi::c_void>,
            /// The widest interface generation cloned from the object's vtable.
            vtable: ::core::marker::PhantomData<A>,
            $($(
                $(#[$context_attribute])*
                $context: $context_type,
            )*)?
        }

        // SAFETY: the original pointer identifies an immutable vtable owned
        // by the hooked module, and the remaining fields contain copied
        // vtable data, a zero-sized layout marker, and any context fields,
        // which must themselves be `Send + Sync`.
        unsafe impl<A: Vtable> Send for $hook<A> {}
        unsafe impl<A: Vtable> Sync for $hook<A> {}

        impl<A: Vtable> $hook<A> {
            /// Clones an original COM vtable and applies this hook's patches.
            ///
            /// # Safety
            ///
            /// `original` must be null or point to a vtable spanning at least
            /// [`A::SLOTS`](Vtable::SLOTS) entries.
            pub(crate) unsafe fn clone_from(
                original: *const ::core::ffi::c_void
                $($(, $context: $context_type)*)?
            ) -> Option<Self> {
                let original = ::core::ptr::NonNull::new(
                    original.cast_mut().cast::<::core::ffi::c_void>()
                )?;
                let saved = unsafe { <$storage>::read(original, A::SLOTS) };
                let mut patched = Box::new(unsafe { <$storage>::read(original, A::SLOTS) });
                let $patched = patched.as_mut();
                $patch
                Some(Self {
                    patched,
                    saved,
                    original,
                    vtable: ::core::marker::PhantomData,
                    $($($context,)*)?
                })
            }
        }

        impl<A: Vtable> $crate::VtableHook for $hook<A> {
            fn slots(&self) -> usize {
                A::SLOTS
            }

            fn vtable_type(&self) -> ::core::any::TypeId {
                ::core::any::TypeId::of::<A>()
            }

            fn patched_vtable(&self) -> *const ::core::ffi::c_void {
                ::core::ptr::from_ref(self.patched.as_ref()).cast()
            }

            fn original_vtable(&self) -> *const ::core::ffi::c_void {
                self.original.as_ptr().cast_const()
            }
        }

        impl<A: Vtable> $hook_trait for $hook<A> {
            $($(
                fn $context(&self) -> &$context_type {
                    &self.$context
                }
            )*)?
            $crate::impl_com_hook!(@methods $($methods)*);
        }
    };

    (@install_widest $unknown:ident; [$($common:ident: $common_type:ty)?] [$($context:ident: $context_type:ty),*];) => { false };
    (@install_widest $unknown:ident; [$($common:ident: $common_type:ty)?] [$($context:ident: $context_type:ty),*]; $interface:path $(, $rest:path)*) => {{
        if let Ok(interface) = <::windows::core::IUnknown as ::windows::core::Interface>::cast::<$interface>($unknown) {
            let pointer = ::windows::core::Interface::as_raw(&interface);
            return unsafe { Self::install_object::<$interface>(pointer, $($common,)? $($context),*) };
        }
        $crate::impl_com_hook!(@install_widest $unknown; [$($common: $common_type)?] [$($context: $context_type),*]; $($rest),*)
    }};

    (@install_all $unknown:ident, $installed:ident, $pointers:ident; [$($common:ident: $common_type:ty)?] [$($context:ident: $context_type:ty),*];) => {};
    (@install_all $unknown:ident, $installed:ident, $pointers:ident; [$($common:ident: $common_type:ty)?] [$($context:ident: $context_type:ty),*]; $interface:path $(, $rest:path)*) => {
        if let Ok(interface) = <::windows::core::IUnknown as ::windows::core::Interface>::cast::<$interface>($unknown) {
            if let Some(pointer) = ::core::ptr::NonNull::new(
                ::windows::core::Interface::as_raw(&interface)
            ) {
                if !$pointers.contains(&pointer) {
                    $pointers.push(pointer);
                    $installed |= unsafe { Self::install_object::<$interface>(pointer.as_ptr(), $($common.clone(),)? $($context.clone()),*) };
                }
            }
        }
        $crate::impl_com_hook!(@install_all $unknown, $installed, $pointers; [$($common: $common_type)?] [$($context: $context_type),*]; $($rest),*);
    };

    (@install_interface $object:ident, $interface_id:ident; [$($common:ident: $common_type:ty)?] [$($context:ident: $context_type:ty),*];) => { false };
    (@install_interface $object:ident, $interface_id:ident; [$($common:ident: $common_type:ty)?] [$($context:ident: $context_type:ty),*]; $interface:path $(, $rest:path)*) => {{
        if *$interface_id == <$interface as ::windows::core::Interface>::IID {
            return unsafe { Self::install_object::<$interface>($object, $($common,)? $($context),*) };
        }
        $crate::impl_com_hook!(@install_interface $object, $interface_id; [$($common: $common_type)?] [$($context: $context_type),*]; $($rest),*)
    }};

    (@methods) => {};

    (@inherent_methods) => {};

    (@inherent_methods
        #[base = $vtable_interface:ident::$base:ident]
        fn $method:ident(&self) -> $result:ty;
        $($rest:tt)*
    ) => {
        pub(crate) fn $method(&self) -> $result {
            unsafe {
                ::core::mem::transmute(
                    (*::core::ptr::from_ref(&self.saved).cast::<<$vtable_interface as ::windows::core::Interface>::Vtable>()).$base
                )
            }
        }
        $crate::impl_com_hook!(@inherent_methods $($rest)*);
    };

    (@methods
        #[base = $vtable_interface:ident::$base:ident]
        fn $method:ident(&self) -> Option<$result:ty>;
        $($rest:tt)*
    ) => {
        fn $method(&self) -> Option<$result> {
            A::supports(::core::any::TypeId::of::<$vtable_interface>())
                .then(|| unsafe {
                    ::core::mem::transmute(
                        (*::core::ptr::from_ref(&self.saved).cast::<<$vtable_interface as ::windows::core::Interface>::Vtable>()).$base
                    )
                })
        }
        $crate::impl_com_hook!(@methods $($rest)*);
    };

    (@methods
        #[base = $vtable_interface:ident::$base:ident]
        fn $method:ident(&self) -> $result:ty;
        $($rest:tt)*
    ) => {
        fn $method(&self) -> $result {
            unsafe {
                ::core::mem::transmute(
                    (*::core::ptr::from_ref(&self.saved).cast::<<$vtable_interface as ::windows::core::Interface>::Vtable>()).$base
                )
            }
        }
        $crate::impl_com_hook!(@methods $($rest)*);
    };
}
