mod com_hook;
mod com_proxy;
mod function_table;

#[macro_export]
macro_rules! vtable {
    ($([$($interface:path),+ $(,)?]),+ $(,)?) => {
        pub(crate) trait Vtable: 'static {
            const SLOTS: usize;

            fn slots() -> usize { Self::SLOTS }
            fn supports(vtable: ::core::any::TypeId) -> bool;
            fn supported_by(vtable: ::core::any::TypeId) -> bool;
        }

        $($crate::vtable!(@hierarchy [] $($interface),+);)+
    };

    (@types $([$($vtbl:ty),+ $(,)?]),+ $(,)?) => {
        pub(crate) trait Vtable: 'static {
            const SLOTS: usize;

            fn slots() -> usize {
                Self::SLOTS
            }

            fn supports(vtable: ::core::any::TypeId) -> bool;

            fn supported_by(vtable: ::core::any::TypeId) -> bool;
        }

        $($crate::vtable!(@raw_hierarchy [] $($vtbl),+);)+
    };

    (@hierarchy [$($base:ty,)*] $vtbl:ty $(, $derived:ty)*) => {
        impl Vtable for $vtbl {
            const SLOTS: usize =
                ::core::mem::size_of::<<$vtbl as ::windows::core::Interface>::Vtable>()
                    / ::core::mem::size_of::<*const ::core::ffi::c_void>();

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

    (@raw_hierarchy [$($base:ty,)*] $vtbl:ty $(, $derived:ty)*) => {
        impl Vtable for $vtbl {
            const SLOTS: usize =
                ::core::mem::size_of::<$vtbl>()
                    / ::core::mem::size_of::<*const ::core::ffi::c_void>();

            fn supports(vtable: ::core::any::TypeId) -> bool {
                vtable == ::core::any::TypeId::of::<$vtbl>()
                    $(|| vtable == ::core::any::TypeId::of::<$base>())*
            }

            fn supported_by(vtable: ::core::any::TypeId) -> bool {
                vtable == ::core::any::TypeId::of::<$vtbl>()
                    $(|| vtable == ::core::any::TypeId::of::<$derived>())*
            }
        }

        $crate::vtable!(@raw_hierarchy_next [$($base,)* $vtbl,] $($derived),*);
    };

    (@raw_hierarchy_next [$($base:ty,)*]) => {};
    (@raw_hierarchy_next [$($base:ty,)*] $vtbl:ty $(, $derived:ty)*) => {
        $crate::vtable!(@raw_hierarchy [$($base,)*] $vtbl $(, $derived)*);
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
        $visibility:vis union $name:ident for $interface:path;
    ) => {
        $crate::com_vtable! {
            @type
            $(#[$attribute])*
            $visibility union $name for <$interface as ::windows::core::Interface>::Vtable;
        }
    };

    (
        @type
        $(#[$attribute:meta])*
        $visibility:vis union $name:ident for $capacity:ty;
    ) => {
        $(#[$attribute])*
        #[repr(C)]
        $visibility union $name {
            raw: [*const ::core::ffi::c_void;
                ::core::mem::size_of::<$capacity>() / ::core::mem::size_of::<*const ::core::ffi::c_void>()],
            capacity: ::core::mem::ManuallyDrop<$capacity>,
        }

        const _: () = assert!(
            ::core::mem::size_of::<$name>() == ::core::mem::size_of::<$capacity>()
        );
        const _: () = assert!(
            ::core::mem::size_of::<$capacity>()
                % ::core::mem::size_of::<*const ::core::ffi::c_void>() == 0
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
                            / ::core::mem::size_of::<*const ::core::ffi::c_void>()
                );
                let mut cloned = Self {
                    raw: [::core::ptr::null(); ::core::mem::size_of::<$capacity>()
                        / ::core::mem::size_of::<*const ::core::ffi::c_void>()],
                };
                unsafe {
                    ::core::ptr::copy_nonoverlapping(
                        original.as_ptr().cast::<*const ::core::ffi::c_void>().cast_const(),
                        (&raw mut cloned.raw).cast::<*const ::core::ffi::c_void>(),
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
/// Given an object and an interface list ordered widest-to-narrowest,
/// this expands to a `Vec<(*mut c_void, $install_fn)>` with one entry per
/// distinct object pointer: the widest interface that resolves to that pointer,
/// paired with `$install::<Interface>` for its generation. Callers that hook every
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
        $($interface:ty),+ $(,)?
    ) => {{
        use ::windows::core::Interface as _;
        let object = $object;
        let mut interfaces: ::std::vec::Vec<(*mut ::core::ffi::c_void, $install_fn)> =
            ::std::vec::Vec::new();
        $(
            if let ::core::result::Result::Ok(interface) = object.cast::<$interface>() {
                let pointer = interface.as_raw();
                if !interfaces.iter().any(|(existing, _)| *existing == pointer) {
                    interfaces.push((pointer, $install::<$interface> as $install_fn));
                }
            }
        )+
        interfaces
    }};
}

/// The common layout-erased view of a cloned COM vtable.
pub trait VtableHook: Send + Sync {
    /// The number of slots spanned by the cloned layout.
    fn slots(&self) -> usize;

    /// The concrete interface-vtable layout used for the clone.
    fn vtable_type(&self) -> ::core::any::TypeId;

    /// The cloned and patched vtable installed on the COM object.
    fn patched_vtable(&self) -> *const ::core::ffi::c_void;

    /// The original vtable restored while releasing the COM object.
    fn original_vtable(&self) -> *const ::core::ffi::c_void;
}

/// ABI-transparent storage used by [`impl_com_proxy!`].
#[doc(hidden)]
#[repr(C)]
pub struct ComProxy {
    vtable: *const ::core::ffi::c_void,
    inner: *mut ::core::ffi::c_void,
    references: ::core::sync::atomic::AtomicU32,
    iid: windows_core::GUID,
    forwarding: Box<[::core::ptr::NonNull<::core::ffi::c_void>]>,
}

unsafe impl Send for ComProxy {}
unsafe impl Sync for ComProxy {}

impl ComProxy {
    /// Builds a proxy whose untouched slots replace `this` and tail-call the
    /// corresponding slot on `inner`.
    pub unsafe fn new(
        inner: *mut ::core::ffi::c_void,
        iid: windows_core::GUID,
        slots: usize,
    ) -> Option<*mut ::core::ffi::c_void> {
        if inner.is_null() || slots > COM_PROXY_FORWARDER_CAPACITY {
            return None;
        }
        let mut forwarding: Box<[::core::ptr::NonNull<::core::ffi::c_void>]> =
            (0..slots)
                .map(|slot| unsafe {
                    ::core::ptr::NonNull::new_unchecked(
                        com_proxy_forwarder(slot) as *const () as *mut _
                    )
                })
                .collect();
        forwarding[0] = unsafe {
            ::core::ptr::NonNull::new_unchecked(com_proxy_query_interface as *const () as *mut _)
        };
        forwarding[1] = unsafe {
            ::core::ptr::NonNull::new_unchecked(com_proxy_add_ref as *const () as *mut _)
        };
        forwarding[2] = unsafe {
            ::core::ptr::NonNull::new_unchecked(com_proxy_release as *const () as *mut _)
        };
        let mut proxy = Box::new(Self {
            vtable: forwarding.as_ptr().cast(),
            inner,
            references: ::core::sync::atomic::AtomicU32::new(1),
            iid,
            forwarding,
        });
        let pointer = ::core::ptr::from_mut(proxy.as_mut()).cast();
        let _ = Box::into_raw(proxy);
        Some(pointer)
    }

    pub unsafe fn abandon(pointer: *mut ::core::ffi::c_void) -> *mut ::core::ffi::c_void {
        let proxy = unsafe { Box::from_raw(pointer.cast::<Self>()) };
        let inner = proxy.inner;
        drop(proxy);
        inner
    }
}

unsafe extern "system" fn com_proxy_query_interface(
    this: *mut ::core::ffi::c_void,
    iid: *const windows_core::GUID,
    output: *mut *mut ::core::ffi::c_void,
) -> i32 {
    if iid.is_null() || output.is_null() {
        return 0x8007_0057u32 as i32;
    }
    let proxy = unsafe { &*this.cast::<ComProxy>() };
    let unknown = <windows_core::IUnknown as windows_core::Interface>::IID;
    if unsafe { *iid } != unknown && unsafe { *iid } != proxy.iid {
        unsafe { *output = ::core::ptr::null_mut() };
        return 0x8000_4002u32 as i32;
    }
    unsafe { *output = this };
    unsafe { com_proxy_add_ref(this) };
    0
}

unsafe extern "system" fn com_proxy_add_ref(this: *mut ::core::ffi::c_void) -> u32 {
    unsafe { &*this.cast::<ComProxy>() }
        .references
        .fetch_add(1, ::core::sync::atomic::Ordering::Relaxed)
        + 1
}

unsafe extern "system" fn com_proxy_release(this: *mut ::core::ffi::c_void) -> u32 {
    let proxy = unsafe { &*this.cast::<ComProxy>() };
    let remaining = proxy
        .references
        .fetch_sub(1, ::core::sync::atomic::Ordering::Release)
        - 1;
    if remaining != 0 {
        return remaining;
    }
    ::core::sync::atomic::fence(::core::sync::atomic::Ordering::Acquire);
    let inner = proxy.inner;
    drop(unsafe { <windows_core::IUnknown as windows_core::Interface>::from_raw(inner) });
    unsafe { drop(Box::from_raw(this.cast::<ComProxy>())) };
    0
}

const COM_PROXY_FORWARDER_CAPACITY: usize = 64;

#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
unsafe extern "system" fn com_proxy_forward<const SLOT: usize>() {
    ::core::arch::naked_asm!(
        "mov rcx, [rcx + 8]",
        "mov rax, [rcx]",
        "jmp qword ptr [rax + {offset}]",
        offset = const SLOT * ::core::mem::size_of::<*const ::core::ffi::c_void>(),
    )
}

#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "system" fn com_proxy_forward<const SLOT: usize>() {
    ::core::arch::naked_asm!(
        "ldr x0, [x0, #8]",
        "ldr x16, [x0]",
        "ldr x16, [x16, #{offset}]",
        "br x16",
        offset = const SLOT * ::core::mem::size_of::<*const ::core::ffi::c_void>(),
    )
}

seq_macro::seq!(SLOT in 0..64 {
    fn com_proxy_forwarder(slot: usize) -> unsafe extern "system" fn() {
        match slot {
            #(SLOT => com_proxy_forward::<SLOT>,)*
            _ => unreachable!("COM proxy slot exceeds forwarding capacity"),
        }
    }
});
