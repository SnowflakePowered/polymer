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

snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system" fn query_interface(
        this: *mut c_void,
        interface_id: *const c_void,
        output: *mut *mut c_void,
    ) -> i32 => crate::hooks::dispatch_query_interface;
}

snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system" fn present(
        this: *mut c_void,
        sync_interval: u32,
        flags: u32,
    ) -> i32 => crate::hooks::dispatch_present;
}

snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system" fn present1(
        this: *mut c_void,
        sync_interval: u32,
        flags: u32,
        parameters: *const c_void,
    ) -> i32 => crate::hooks::dispatch_present1;
}

snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system" fn factory_create_swapchain(
        this: *mut c_void,
        device: *mut c_void,
        desc: *const c_void,
        swapchain: *mut *mut c_void,
    ) -> i32 => crate::hooks::dispatch_factory_create_swapchain;
}

snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system" fn factory_create_swapchain_for_hwnd(
        this: *mut c_void,
        device: *mut c_void,
        window: *mut c_void,
        desc: *const c_void,
        fullscreen_desc: *const c_void,
        restrict_to_output: *mut c_void,
        swapchain: *mut *mut c_void,
    ) -> i32 => crate::hooks::dispatch_factory_create_swapchain_for_hwnd;
}

snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system" fn factory_create_swapchain_for_core_window(
        this: *mut c_void,
        device: *mut c_void,
        window: *mut c_void,
        desc: *const c_void,
        restrict_to_output: *mut c_void,
        swapchain: *mut *mut c_void,
    ) -> i32 => crate::hooks::dispatch_factory_create_swapchain_for_core_window;
}

snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system" fn factory_create_swapchain_for_composition(
        this: *mut c_void,
        device: *mut c_void,
        desc: *const c_void,
        restrict_to_output: *mut c_void,
        swapchain: *mut *mut c_void,
    ) -> i32 => crate::hooks::dispatch_factory_create_swapchain_for_composition;
}

snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system" fn resize_buffers(
        this: *mut c_void,
        buffer_count: u32,
        width: u32,
        height: u32,
        format: u32,
        flags: u32,
    ) -> i32 => crate::hooks::dispatch_resize_buffers;
}

snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system" fn release(
        this: *mut c_void,
    ) -> u32 => crate::hooks::dispatch_release;
}

snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system" fn factory_release(
        this: *mut c_void,
    ) -> u32 => crate::hooks::dispatch_factory_release;
}
