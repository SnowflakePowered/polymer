use std::ffi::c_void;

snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system" fn create_factory(
        interface_id: *const c_void,
        factory: *mut *mut c_void,
    ) -> i32 => crate::hooks::dispatch_create_factory;
}

snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system" fn create_factory1(
        interface_id: *const c_void,
        factory: *mut *mut c_void,
    ) -> i32 => crate::hooks::dispatch_create_factory1;
}

snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system" fn create_factory2(
        flags: u32,
        interface_id: *const c_void,
        factory: *mut *mut c_void,
    ) -> i32 => crate::hooks::dispatch_create_factory2;
}
