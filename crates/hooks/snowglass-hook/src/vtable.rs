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

/// Implements the shared storage lifecycle and saved-slot accessors for a
/// typed COM vtable hook.
///
/// The hook type is expected to contain `patched`, `saved`, `original`, and
/// `vtable` fields. A slot whose return type is `Option<T>` is available only
/// when the clone's concrete vtable type includes the layout named by its
/// `vtable` attribute.
#[macro_export]
macro_rules! impl_com_hook {
    (
        impl $hook_trait:ident for $hook:ident {
            vtable $storage:ty;
            patch |$patched:ident| $patch:block
            $($methods:tt)*
        }
    ) => {
        // SAFETY: the original pointer identifies an immutable vtable owned
        // by the hooked module, and the remaining fields contain only copied
        // vtable data and a zero-sized layout marker.
        unsafe impl<A: Vtable> Send for $hook<A> {}
        unsafe impl<A: Vtable> Sync for $hook<A> {}

        impl<A: Vtable> $hook<A> {
            /// Clones an original COM vtable and applies this hook's patches.
            ///
            /// # Safety
            ///
            /// `original` must be null or point to a vtable spanning at least
            /// [`A::SLOTS`](Vtable::SLOTS) entries.
            pub(crate) unsafe fn clone_from(original: *const usize) -> Option<Self> {
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
            $crate::impl_com_hook!(@methods $($methods)*);
        }
    };

    (@methods) => {};

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

    struct Base;
    struct Derived;
    struct Other;

    crate::vtable! {
        [Base, Derived],
        [Other],
    }

    #[test]
    fn vtable_support_follows_declared_type_hierarchy() {
        assert_eq!(Base::slots(), Base::SLOTS);
        assert!(Derived::supports(TypeId::of::<Base>()));
        assert!(Base::supported_by(TypeId::of::<Derived>()));
        assert!(!Other::supports(TypeId::of::<Base>()));
        assert!(!Base::supported_by(TypeId::of::<Other>()));
    }
}
