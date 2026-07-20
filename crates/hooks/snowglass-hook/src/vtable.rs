#[macro_export]
macro_rules! vtable {
    ($([$($vtbl:ty),+ $(,)?]),+ $(,)?) => {
        pub(crate) trait Vtable: 'static {
            const SLOTS: usize;

            fn slots() -> usize {
                Self::SLOTS
            }

            fn supports(vtable: ::core::any::TypeId) -> bool;

            fn supported_by(vtable: ::core::any::TypeId) -> bool;
        }

        $($crate::vtable!(@hierarchy [] $($vtbl),+);)+
    };

    (@hierarchy [$($base:ty,)*] $vtbl:ty $(, $derived:ty)*) => {
        impl Vtable for $vtbl {
            const SLOTS: usize =
                ::core::mem::size_of::<$vtbl>() / ::core::mem::size_of::<usize>();

            fn supports(vtable: ::core::any::TypeId) -> bool {
                vtable == ::core::any::TypeId::of::<$vtbl>()
                    $(|| vtable == ::core::any::TypeId::of::<$base>())*
            }

            fn supported_by(vtable: ::core::any::TypeId) -> bool {
                vtable == ::core::any::TypeId::of::<$vtbl>()
                    $(|| vtable == ::core::any::TypeId::of::<$derived>())*
            }
        }

        $crate::vtable!(@hierarchy_next [$($base,)* $vtbl,] $($derived),*);
    };

    (@hierarchy_next [$($base:ty,)*]) => {};
    (@hierarchy_next [$($base:ty,)*] $vtbl:ty $(, $derived:ty)*) => {
        $crate::vtable!(@hierarchy [$($base,)*] $vtbl $(, $derived)*);
    };
}

/// Declares fixed-capacity storage for an opaque copy of a COM vtable.
///
/// The capacity type supplies the allocation size and alignment. Narrower
/// interface generations are prefixes and can be projected with `as_ptr` or
/// `as_mut_ptr` after their slots have been copied into the storage.
#[macro_export]
macro_rules! com_vtable {
    (
        $(#[$attribute:meta])*
        $visibility:vis union $name:ident for $capacity:ty;
    ) => {
        $(#[$attribute])*
        #[repr(C)]
        $visibility union $name {
            raw: [usize; ::core::mem::size_of::<$capacity>() / ::core::mem::size_of::<usize>()],
            capacity: ::core::mem::ManuallyDrop<$capacity>,
        }

        const _: () = assert!(
            ::core::mem::size_of::<$name>() == ::core::mem::size_of::<$capacity>()
        );
        const _: () = assert!(
            ::core::mem::size_of::<$capacity>() % ::core::mem::size_of::<usize>() == 0
        );

        impl $name {
            /// Copies `slots` opaque vtable entries into zero-initialized
            /// fixed-capacity storage.
            ///
            /// # Safety
            ///
            /// `original` must point to at least `slots` entries.
            unsafe fn read(
                original: ::core::ptr::NonNull<::core::ffi::c_void>,
                slots: usize,
            ) -> Self {
                assert!(
                    slots
                        <= ::core::mem::size_of::<$capacity>()
                            / ::core::mem::size_of::<usize>()
                );
                let mut cloned = Self {
                    raw: [0; ::core::mem::size_of::<$capacity>()
                        / ::core::mem::size_of::<usize>()],
                };
                unsafe {
                    ::core::ptr::copy_nonoverlapping(
                        original.as_ptr().cast::<usize>().cast_const(),
                        (&raw mut cloned.raw).cast::<usize>(),
                        slots,
                    );
                }
                cloned
            }

            fn as_ptr<T>(&self) -> *const T {
                (&raw const self.raw).cast()
            }

            fn as_mut_ptr<T>(&mut self) -> *mut T {
                (&raw mut self.raw).cast()
            }
        }
    };
}

/// Resolves the distinct COM interfaces an object exposes, widest first, each
/// paired with a monomorphized install routine for that interface's vtable
/// layout.
///
/// A COM object can implement several generations of an interface, and
/// `QueryInterface` may hand out a distinct pointer per generation (aggregated
/// objects) or one shared pointer (tear-off-free objects). A hook has to clone
/// the *widest* generation each pointer implements, so that a later
/// `QueryInterface` to a wider interface does not dispatch through the
/// uninitialized tail of a clone sized to a narrower generation.
///
/// Given an object and an `Interface => Vtbl` list ordered widest-to-narrowest,
/// this expands to a `Vec<(*mut c_void, $install_fn)>` with one entry per
/// distinct object pointer: the widest interface that resolves to that pointer,
/// paired with `$install::<Vtbl>` for its generation. Callers that hook every
/// interface install into each entry; callers that want only the widest take
/// the first entry. Include the base interface as the final list entry when its
/// pointer should be covered as a fallback.
///
/// `$object` must be a reference to a live COM interface (anything implementing
/// [`windows::core::Interface`]); `$install` names a routine generic over a
/// [`Vtable`] layout; `$install_fn` is the shared function-pointer type that
/// routine coerces to.
#[macro_export]
macro_rules! widest_interfaces {
    (
        from $object:expr => $install_fn:ty;
        install $install:ident;
        $($interface:ty => $vtbl:ty),+ $(,)?
    ) => {{
        use ::windows::core::Interface as _;
        let object = $object;
        let mut interfaces: ::std::vec::Vec<(*mut ::core::ffi::c_void, $install_fn)> =
            ::std::vec::Vec::new();
        $(
            if let ::core::result::Result::Ok(interface) = object.cast::<$interface>() {
                let pointer = interface.as_raw();
                if !interfaces.iter().any(|(existing, _)| *existing == pointer) {
                    interfaces.push((pointer, $install::<$vtbl> as $install_fn));
                }
            }
        )+
        interfaces
    }};
}

/// Declares a void-returning COM interception terminal that forwards to an
/// optional original method before running its observation block.
#[macro_export]
macro_rules! com_trampoline {
    (
        $(#[$attribute:meta])*
        $visibility:vis unsafe extern "system" fn $name:ident(
            $($argument:ident: $argument_type:ty),* $(,)?
        );
        original $original:expr;
        after $after:block
    ) => {
        $(#[$attribute])*
        $visibility unsafe extern "system" fn $name(
            $($argument: $argument_type),*
        ) {
            if let Some(original) = $original {
                unsafe { original($($argument),*) };
            }
            $after
        }
    };
}

/// The common layout-erased view of a cloned COM vtable.
pub trait VtableHook: Send + Sync {
    /// The number of slots spanned by the cloned layout.
    fn slots(&self) -> usize;

    /// The concrete interface-vtable layout used for the clone.
    fn vtable_type(&self) -> ::core::any::TypeId;

    /// The cloned and patched vtable installed on the COM object.
    fn patched_vtable(&self) -> *const usize;

    /// The original vtable restored while releasing the COM object.
    fn original_vtable(&self) -> *const usize;
}

/// Declares typed around-hook chains for selected entries in a mutable
/// function-pointer table. The generated dispatcher for each entry is written
/// into the table, while the captured base function is the chain terminal.
#[macro_export]
macro_rules! function_table_hook {
    (
        #[resolve = $resolve:path]
        $visibility:vis struct $table_hook:ident {
            const MINIMUM_SLOTS: $minimum_slots_type:ty = $minimum_slots:expr;

            $(
                #[slot = $slot:expr]
                context struct {
                    $($context_field:ident: $context_field_type:ty = $context_field_value:expr),* $(,)?
                }
                unsafe extern $abi:literal fn $dispatch:ident(
                    $($argument:ident: $argument_type:ty),* $(,)?
                ) $(-> $result:ty)?;
            )+
        }
    ) => { $crate::__paste! {
        $visibility struct $table_hook {
            $($dispatch: unsafe extern $abi fn($($argument_type),*) $(-> $result)?,)+
        }

        impl $table_hook {
            $visibility const MINIMUM_SLOTS: $minimum_slots_type = $minimum_slots;

            /// Captures the base functions selected by this hook.
            ///
            /// # Safety
            /// `table` must point to `table_bytes` bytes of readable function
            /// table storage whose declared entries have the stated ABIs.
            $visibility unsafe fn capture(
                table: *mut ::core::ffi::c_void,
                table_bytes: u32,
            ) -> Result<Self, &'static str> {
                if table.is_null() {
                    return Err("function table is null");
                }
                let slots = table_bytes as usize / ::core::mem::size_of::<usize>();
                if slots < Self::MINIMUM_SLOTS as usize {
                    return Err("function table is too small");
                }
                let table = table.cast::<usize>();
                $(
                    let address = unsafe { table.add($slot).read() };
                    if address == 0 {
                        return Err("function table contains a null hooked entry");
                    }
                    let $dispatch = unsafe {
                        ::core::mem::transmute_copy::<
                            usize,
                            unsafe extern $abi fn($($argument_type),*) $(-> $result)?,
                        >(&address)
                    };
                )+
                Ok(Self { $($dispatch,)+ })
            }

            /// Replaces the selected entries with their generated dispatchers.
            ///
            /// # Safety
            /// `table` must point to `table_bytes` bytes of writable function
            /// table storage with the same layout passed to `capture`.
            $visibility unsafe fn patch(
                &self,
                table: *mut ::core::ffi::c_void,
                table_bytes: u32,
            ) -> Result<(), &'static str> {
                if table.is_null() {
                    return Err("function table is null");
                }
                let slots = table_bytes as usize / ::core::mem::size_of::<usize>();
                if slots < Self::MINIMUM_SLOTS as usize {
                    return Err("function table is too small");
                }
                let table = table.cast::<usize>();
                $(unsafe { table.add($slot).write($dispatch as usize) };)+
                Ok(())
            }

            $($visibility fn $dispatch(
                &self,
            ) -> unsafe extern $abi fn($($argument_type),*) $(-> $result)? {
                self.$dispatch
            })+
        }

        $(
            $visibility type [<$dispatch:camel Hook>] = unsafe fn(
                $($argument_type,)*
                [<$dispatch:camel Context>]<'_>,
            ) $(-> $result)?;

            static [<$dispatch:upper _HOOKS>]: ::std::sync::LazyLock<
                ::std::sync::RwLock<Vec<[<$dispatch:camel Hook>]> >
            > = ::std::sync::LazyLock::new(|| ::std::sync::RwLock::new(Vec::new()));

            $visibility struct [<$dispatch:camel Context>]<'a> {
                hooks: ::std::slice::Iter<'a, [<$dispatch:camel Hook>]>,
                terminal: &'a [<$dispatch:camel Hook>],
                base: unsafe extern $abi fn($($argument_type),*) $(-> $result)?,
                $($visibility $context_field: $context_field_type,)*
            }

            impl<'a> $crate::HookChain<'a, [<$dispatch:camel Hook>]>
                for [<$dispatch:camel Context>]<'a>
            {
                fn fp_next(&mut self) -> Option<&'a [<$dispatch:camel Hook>]> {
                    Some(self.hooks.next().unwrap_or(self.terminal))
                }
            }

            $visibility fn [<register_ $dispatch>](function: [<$dispatch:camel Hook>]) {
                let mut chain = [<$dispatch:upper _HOOKS>]
                    .write()
                    .unwrap_or_else(|error| error.into_inner());
                if !chain.iter().any(|registered| *registered as usize == function as usize) {
                    chain.push(function);
                }
            }

            unsafe extern $abi fn $dispatch(
                $($argument: $argument_type),*
            ) $(-> $result)? {
                unsafe fn terminal(
                    $($argument: $argument_type,)*
                    context: [<$dispatch:camel Context>]<'_>,
                ) $(-> $result)? {
                    unsafe { (context.base)($($argument),*) }
                }

                let base = $resolve().$dispatch();
                let chain = [<$dispatch:upper _HOOKS>]
                    .read()
                    .unwrap_or_else(|error| error.into_inner());
                let mut hooks = chain.iter();
                let terminal: [<$dispatch:camel Hook>] = terminal;
                let next = *hooks.next().unwrap_or(&terminal);
                unsafe {
                    next(
                        $($argument,)*
                        [<$dispatch:camel Context>] {
                            hooks,
                            terminal: &terminal,
                            base,
                            $($context_field: $context_field_value,)*
                        },
                    )
                }
            }
        )+
    }};
}

/// Declares a typed COM vtable hook and implements its shared storage
/// lifecycle and saved-slot accessors.
///
/// The macro emits the hook struct itself: the patched clone, the saved
/// original slots, the original vtable pointer, and (for the generic form)
/// the cloned-layout marker. A slot whose return type is `Option<T>` is
/// available only when the clone's concrete vtable type includes the layout
/// named by its `vtable` attribute.
///
/// An optional `context` clause declares additional fields carried by the
/// hook, mirroring the field-list syntax of the detour chain contexts. Each
/// field becomes a `clone_from` parameter in declaration order and is exposed
/// through a same-named accessor, keeping per-object state owned by the hook
/// itself rather than a parallel registry entry.
#[macro_export]
macro_rules! impl_com_hook {
    (
        impl $hook:ident {
            vtable $storage:ty;
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
                original: *const usize
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
            pub(crate) fn patched_vtable(&self) -> *const usize {
                ::core::ptr::from_ref(self.patched.as_ref()).cast()
            }

            /// The object's original vtable pointer.
            pub(crate) fn original_vtable(&self) -> *const usize {
                self.original.as_ptr().cast()
            }

            $crate::impl_com_hook!(@inherent_methods $($methods)*);
        }
    };

    (
        impl $hook_trait:ident for $hook:ident {
            vtable $storage:ty;
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
                original: *const usize
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

            fn patched_vtable(&self) -> *const usize {
                ::core::ptr::from_ref(self.patched.as_ref()).cast()
            }

            fn original_vtable(&self) -> *const usize {
                self.original.as_ptr().cast()
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

    (@methods) => {};

    (@inherent_methods) => {};

    (@inherent_methods
        #[vtable = $vtable:ty]
        #[base = $base:ident]
        fn $method:ident(&self) -> $result:ty;
        $($rest:tt)*
    ) => {
        pub(crate) fn $method(&self) -> $result {
            unsafe {
                ::core::mem::transmute(
                    (*::core::ptr::from_ref(&self.saved).cast::<$vtable>()).$base
                )
            }
        }
        $crate::impl_com_hook!(@inherent_methods $($rest)*);
    };

    (@methods
        #[vtable = $vtable:ty]
        #[base = $base:ident]
        fn $method:ident(&self) -> Option<$result:ty>;
        $($rest:tt)*
    ) => {
        fn $method(&self) -> Option<$result> {
            A::supports(::core::any::TypeId::of::<$vtable>())
                .then(|| unsafe {
                    ::core::mem::transmute(
                        (*::core::ptr::from_ref(&self.saved).cast::<$vtable>()).$base
                    )
                })
        }
        $crate::impl_com_hook!(@methods $($rest)*);
    };

    (@methods
        #[vtable = $vtable:ty]
        #[base = $base:ident]
        fn $method:ident(&self) -> $result:ty;
        $($rest:tt)*
    ) => {
        fn $method(&self) -> $result {
            unsafe {
                ::core::mem::transmute(
                    (*::core::ptr::from_ref(&self.saved).cast::<$vtable>()).$base
                )
            }
        }
        $crate::impl_com_hook!(@methods $($rest)*);
    };
}

#[cfg(test)]
mod tests {
    use std::any::TypeId;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Base;
    struct Derived;
    struct Other;

    #[repr(C)]
    struct Capacity {
        first: usize,
        second: usize,
    }

    crate::vtable! {
        [Base, Derived],
        [Other],
    }

    crate::com_vtable! {
        union TestVtable for Capacity;
    }

    static TRAMPOLINE_CALLS: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "system" fn original_trampoline(value: usize) {
        TRAMPOLINE_CALLS.store(value, Ordering::Relaxed);
    }

    crate::com_trampoline! {
        unsafe extern "system" fn test_trampoline(value: usize);
        original Some(original_trampoline as unsafe extern "system" fn(usize));
        after {
            TRAMPOLINE_CALLS
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| Some(value * 10))
                .unwrap();
        }
    }

    #[test]
    fn vtable_support_follows_declared_type_hierarchy() {
        assert_eq!(Base::slots(), Base::SLOTS);
        assert!(Derived::supports(TypeId::of::<Base>()));
        assert!(Base::supported_by(TypeId::of::<Derived>()));
        assert!(!Other::supports(TypeId::of::<Base>()));
        assert!(!Base::supported_by(TypeId::of::<Other>()));
    }

    #[test]
    fn com_vtable_copies_and_projects_opaque_slots() {
        let original = [11usize, 22];
        let pointer = std::ptr::NonNull::new(original.as_ptr().cast_mut().cast()).unwrap();
        let mut storage = unsafe { TestVtable::read(pointer, original.len()) };

        assert_eq!(unsafe { (*storage.as_ptr::<Capacity>()).second }, 22);
        unsafe { (*storage.as_mut_ptr::<Capacity>()).first = 33 };
        assert_eq!(unsafe { (*storage.as_ptr::<Capacity>()).first }, 33);
    }

    #[test]
    fn com_trampoline_forwards_before_observing() {
        TRAMPOLINE_CALLS.store(0, Ordering::Relaxed);
        unsafe { test_trampoline(2) };
        assert_eq!(TRAMPOLINE_CALLS.load(Ordering::Relaxed), 20);
    }
}
