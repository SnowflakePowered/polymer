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

/// Declares typed around-hook chains for selected entries in a mutable
/// function-pointer table. The generated dispatcher for each entry is written
/// into the table, while the captured base function is the chain terminal.
/// A managed hook declared with `#[state: Type = value]` injects `state` into
/// every dispatch context and returns its shared `Arc<Type>` from `install`.
#[macro_export]
macro_rules! function_table_hook {
    (
        $visibility:vis struct $table_hook:ident {
            $($body:tt)*
        }
    ) => {
        $crate::function_table_hook! {
            #[state: () = ()]
            $visibility struct $table_hook { $($body)* }
        }
    };

    (
        #[state: $state:ty = $state_value:expr]
        $visibility:vis struct $table_hook:ident {
            const MINIMUM_SLOTS: $minimum_slots_type:ty = $minimum_slots:expr;
            $(
                #[slot = $slot:expr]
                $(#[prefix($($prefix:tt)*)])?
                $(#[infix($($infix:path),* $(,)? )])?
                $(#[postfix($($postfix:tt)*)])?
                unsafe extern $abi:literal fn $dispatch:ident(
                    $($argument:ident: $argument_type:ty),* $(,)?
                ) $(-> $result:ty)?
                $( { #[context] struct {
                    $($local:ident: $local_type:ty = $local_value:expr),* $(,)?
                } } )? $(;)?
            )+
        }
    ) => { $crate::__paste! {
        $crate::function_table_hook! {
            #[resolve = [<__resolve_ $table_hook:snake>]]
            $visibility struct $table_hook {
                const MINIMUM_SLOTS: $minimum_slots_type = $minimum_slots;
                $(
                    #[slot = $slot]
                    $(#[prefix($($prefix)*)])?
                    $(#[infix($($infix),*)])?
                    $(#[postfix($($postfix)*)])?
                    unsafe extern $abi fn $dispatch(
                        $($argument: $argument_type),*
                    ) $(-> $result)? {
                        #[context] struct {
                            state: ::std::sync::Arc<$state> =
                                [<__ $table_hook _registry>].lock()
                                    .as_ref()
                                    .map(|installation| installation.state.clone())
                                    .expect("function table hook is not installed"),
                            $($($local: $local_type = $local_value,)*)?
                        }
                    }
                )+
            }
        }

        struct [<$table_hook Installation>] {
            hook: ::std::sync::Arc<$table_hook>,
            tables: ::std::vec::Vec<($crate::__PointerKey, u32)>,
            state: ::std::sync::Arc<$state>,
        }

        #[allow(non_upper_case_globals)]
        static [<__ $table_hook _registry>]:
            $crate::__Mutex<Option<[<$table_hook Installation>]>> =
            $crate::__Mutex::new(None);

        fn [<__resolve_ $table_hook:snake>]() -> ::std::sync::Arc<$table_hook> {
            [<__ $table_hook _registry>].lock()
                .as_ref().map(|installation| installation.hook.clone())
                .expect("function table hook is not installed")
        }

        impl $table_hook {
            /// Captures and patches a function table, reusing the captured
            /// base functions for later publications of the same API table.
            ///
            /// # Safety
            /// `table` must satisfy the generated table hook's capture and
            /// patch requirements.
            $visibility unsafe fn install(
                table: *mut ::core::ffi::c_void,
                table_bytes: u32,
            ) -> Result<::std::sync::Arc<$state>, &'static str> {
                let mut registry = [<__ $table_hook _registry>].lock();
                if registry.is_none() {
                    let hook = ::std::sync::Arc::new(unsafe {
                        $table_hook::capture(table, table_bytes)?
                    });
                    *registry = Some([<$table_hook Installation>] {
                        hook,
                        tables: ::std::vec::Vec::new(),
                        state: ::std::sync::Arc::new($state_value),
                    });
                }
                let installation = registry.as_mut().expect("function table installation exists");
                unsafe { installation.hook.patch(table, table_bytes)? };
                let address = $crate::__PointerKey::new(table)
                    .expect("function table was validated as non-null");
                if !installation.tables.iter().any(|entry| entry.0 == address) {
                    installation.tables.push((address, table_bytes));
                }
                Ok(installation.state.clone())
            }

            $visibility fn with_state<T>(
                select: impl FnOnce(&$state) -> T,
            ) -> Option<T> {
                [<__ $table_hook _registry>].lock()
                    .as_ref()
                    .map(|installation| select(installation.state.as_ref()))
            }

            $visibility fn uninstall() -> Result<(), &'static str> {
                let mut registry = [<__ $table_hook _registry>].lock();
                let Some(installation) = registry.as_mut() else { return Ok(()); };
                let mut first_error = None;
                let mut index = installation.tables.len();
                while index != 0 {
                    index -= 1;
                    let (address, table_bytes) = installation.tables[index];
                    match unsafe { installation.hook.restore(address.as_ptr(), table_bytes) } {
                        Ok(()) => { installation.tables.remove(index); }
                        Err(error) => { first_error.get_or_insert(error); }
                    }
                }
                if installation.tables.is_empty() { *registry = None; }
                first_error.map_or(Ok(()), Err)
            }
        }
    }};

    (
        #[resolve = $resolve:path]
        $visibility:vis struct $table_hook:ident {
            const MINIMUM_SLOTS: $minimum_slots_type:ty = $minimum_slots:expr;
            #[context]
            $context_visibility:vis struct $common_context:ident {
                $($field_visibility:vis $field:ident: $field_type:ty = $field_value:expr),* $(,)?
            }
            $(
                #[slot = $slot:expr]
                $(#[prefix($($prefix:tt)*)])?
                $(#[infix($($infix:path),* $(,)? )])?
                $(#[postfix($($postfix:tt)*)])?
                unsafe extern $abi:literal fn $dispatch:ident(
                    $($argument:ident: $argument_type:ty),* $(,)?
                ) $(-> $result:ty)?
                $( { #[context] struct {
                    $($local:ident: $local_type:ty = $local_value:expr),* $(,)?
                } } )? $(;)?
            )+
        }
    ) => {
        $context_visibility struct $common_context {
            $($field_visibility $field: $field_type,)*
        }
        $crate::function_table_hook!(@common
            #[resolve = $resolve]
            $visibility struct $table_hook {
                const MINIMUM_SLOTS: $minimum_slots_type = $minimum_slots;
                common context: $common_context = $common_context {
                    $($field: $field_value,)*
                };
                $(
                    #[slot = $slot]
                    $(#[prefix($($prefix)*)])?
                    $(#[infix($($infix),*)])?
                    $(#[postfix($($postfix)*)])?
                    unsafe extern $abi fn $dispatch(
                        $($argument: $argument_type),*
                    ) $(-> $result)?
                    [$(#[context] struct {
                        $($local: $local_type = $local_value),*
                    })?]
                )+
            }
        );
    };
    (@common
        #[resolve = $resolve:path]
        $visibility:vis struct $table_hook:ident {
            const MINIMUM_SLOTS: $minimum_slots_type:ty = $minimum_slots:expr;
            common context: $common_context:ty = $common_value:expr;
            $(
                #[slot = $slot:expr]
                $(#[prefix($($prefix:tt)*)])?
                $(#[infix($($infix:path),* $(,)? )])?
                $(#[postfix($($postfix:tt)*)])?
                unsafe extern $abi:literal fn $dispatch:ident(
                    $($argument:ident: $argument_type:ty),* $(,)?
                ) $(-> $result:ty)?
                [$(#[context] struct {
                    $($local:ident: $local_type:ty = $local_value:expr),* $(,)?
                })?]
            )+
        }
    ) => {
        $crate::function_table_hook! {
            #[resolve = $resolve]
            $visibility struct $table_hook {
                const MINIMUM_SLOTS: $minimum_slots_type = $minimum_slots;
                $(
                    #[slot = $slot]
                    $(#[prefix($($prefix)*)])?
                    $(#[infix($($infix),*)])?
                    $(#[postfix($($postfix)*)])?
                    unsafe extern $abi fn $dispatch(
                        $($argument: $argument_type),*
                    ) $(-> $result)? {
                        #[context] struct {
                            common: $common_context = $common_value,
                            $($($local: $local_type = $local_value,)*)?
                        }
                    }
                )+
            }
        }
    };
    (
        #[resolve = $resolve:path]
        $visibility:vis struct $table_hook:ident {
            const MINIMUM_SLOTS: $minimum_slots_type:ty = $minimum_slots:expr;

            $(
                #[slot = $slot:expr]
                $(#[prefix($($prefix:tt)*)])?
                $(#[infix($($infix:path),* $(,)? )])?
                $(#[postfix($($postfix:tt)*)])?
                unsafe extern $abi:literal fn $dispatch:ident(
                    $($argument:ident: $argument_type:ty),* $(,)?
                ) $(-> $result:ty)? {
                    #[context] struct {
                        $($context_field:ident: $context_field_type:ty = $context_field_value:expr),* $(,)?
                    }
                }
            )+
        }
    ) => { $crate::__paste! {
        $visibility struct $table_hook {
            $($dispatch: unsafe extern $abi fn($($argument_type),*) $(-> $result)?,)+
            $([<$dispatch _hooks>]: $crate::__RwLock<
                Vec<[<$dispatch:camel Hook>]>,
            >,)+
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
                let slots = table_bytes as usize
                    / ::core::mem::size_of::<*mut ::core::ffi::c_void>();
                if slots < Self::MINIMUM_SLOTS as usize {
                    return Err("function table is too small");
                }
                let table = table.cast::<*mut ::core::ffi::c_void>();
                $(
                    let address = unsafe { table.add($slot).read() };
                    if address.is_null() {
                        return Err("function table contains a null hooked entry");
                    }
                    let $dispatch = unsafe {
                        ::core::mem::transmute_copy::<
                            *mut ::core::ffi::c_void,
                            unsafe extern $abi fn($($argument_type),*) $(-> $result)?,
                        >(&address)
                    };
                )+
                Ok(Self {
                    $($dispatch,
                    [<$dispatch _hooks>]: $crate::__RwLock::new(
                        $crate::chain_hooks!(@infix_slice_as [<$dispatch:camel Hook>];
                            $(infix($($infix),*))?
                        ).to_vec()
                    ),)+
                })
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
                let slots = table_bytes as usize
                    / ::core::mem::size_of::<*mut ::core::ffi::c_void>();
                if slots < Self::MINIMUM_SLOTS as usize {
                    return Err("function table is too small");
                }
                let table = table.cast::<*mut ::core::ffi::c_void>();
                $(unsafe {
                    table.add($slot).write(
                        ($dispatch as *const ()).cast_mut().cast::<::core::ffi::c_void>()
                    )
                };)+
                Ok(())
            }

            /// Restores the captured base functions in a previously patched table.
            ///
            /// # Safety
            /// `table` must identify compatible writable table storage.
            #[allow(dead_code)]
            $visibility unsafe fn restore(
                &self,
                table: *mut ::core::ffi::c_void,
                table_bytes: u32,
            ) -> Result<(), &'static str> {
                if table.is_null() {
                    return Err("function table is null");
                }
                let slots = table_bytes as usize
                    / ::core::mem::size_of::<*mut ::core::ffi::c_void>();
                if slots < Self::MINIMUM_SLOTS as usize {
                    return Err("function table is too small");
                }
                let table = table.cast::<*mut ::core::ffi::c_void>();
                $(unsafe {
                    table.add($slot).write(
                        (self.$dispatch as *const ()).cast_mut().cast::<::core::ffi::c_void>()
                    )
                };)+
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

            $visibility struct [<$dispatch:camel Context>]<'a> {
                hooks: ::std::slice::Iter<'a, [<$dispatch:camel Hook>]>,
                terminal: &'a [<$dispatch:camel Hook>],
                base: unsafe extern $abi fn($($argument_type),*) $(-> $result)?,
                $($visibility $context_field: &'a $context_field_type,)*
            }

            impl<'a> $crate::HookChain<'a, [<$dispatch:camel Hook>]>
                for [<$dispatch:camel Context>]<'a>
            {
                fn fp_next(&mut self) -> Option<&'a [<$dispatch:camel Hook>]> {
                    Some(self.hooks.next().unwrap_or(self.terminal))
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

                let table_hook = $resolve();
                let base = table_hook.$dispatch();
                $(let $context_field = $context_field_value;)*
                let chain = table_hook.[<$dispatch _hooks>]
                    .read();
                let mut hooks = chain.iter();
                let terminal: [<$dispatch:camel Hook>] = terminal;
                let _prefix_context = [<$dispatch:camel Context>] {
                    hooks: hooks.clone(),
                    terminal: &terminal,
                    base,
                    $($context_field: &$context_field,)*
                };
                let prefix: &[unsafe fn(
                    $($argument_type,)*
                    &[<$dispatch:camel Context>]<'_>,
                )] = $crate::chain_hooks!(@prefix_slice
                    [$(($argument, $argument_type))*]
                    [[<$dispatch:camel Context>]]
                    [yes]
                    [];
                    $(prefix($($prefix)*))?
                );
                for observer in prefix {
                    unsafe { observer($($argument,)* &_prefix_context) };
                }
                let next = *hooks.next().unwrap_or(&terminal);
                let result = unsafe {
                    next(
                        $($argument,)*
                        [<$dispatch:camel Context>] {
                            hooks,
                            terminal: &terminal,
                            base,
                            $($context_field: &$context_field,)*
                        },
                    )
                };
                let _postfix_context = [<$dispatch:camel Context>] {
                    hooks: [].iter(),
                    terminal: &terminal,
                    base,
                    $($context_field: &$context_field,)*
                };
                let postfix: &[unsafe fn(
                    $($argument_type,)*
                    $crate::function_table_hook!(@result_type $($result)?),
                    &[<$dispatch:camel Context>]<'_>,
                )] = $crate::chain_hooks!(@postfix_slice
                    [$(($argument, $argument_type))*]
                    [$crate::function_table_hook!(@result_type $($result)?)]
                    [[<$dispatch:camel Context>]]
                    [yes]
                    [];
                    $(postfix($($postfix)*))?
                );
                for observer in postfix {
                    unsafe { observer($($argument,)* result, &_postfix_context) };
                }
                result
            }
        )+
    }};

    (@result_type $result:ty) => { $result };
    (@result_type) => { () };
}

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
                $crate::chain_hooks!(@infix_slice_as [<$dispatch:camel Hook>];
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
            )] = $crate::chain_hooks!(@prefix_slice
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
            )] = $crate::chain_hooks!(@postfix_slice
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
                    (*unknown).Release = ::core::mem::transmute(
                        $release_dispatch as unsafe extern "system" fn($release_this_type) -> u32
                    );
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
