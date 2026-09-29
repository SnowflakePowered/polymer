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
                        $crate::__chain_hooks_call!(@infix_slice_as [<$dispatch:camel Hook>];
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
                )] = $crate::__chain_hooks_call!(@prefix_slice
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
                )] = $crate::__chain_hooks_call!(@postfix_slice
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
