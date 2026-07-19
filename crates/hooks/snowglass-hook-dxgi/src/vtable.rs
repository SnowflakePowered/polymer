#![allow(clippy::missing_transmute_annotations)]

use std::ffi::c_void;
use windows::{
    Win32::Graphics::Dxgi::{
        IDXGIFactory_Vtbl, IDXGIFactory1_Vtbl, IDXGIFactory2_Vtbl, IDXGIFactory3_Vtbl,
        IDXGIFactory4_Vtbl, IDXGIFactory5_Vtbl, IDXGIFactory6_Vtbl, IDXGIFactory7_Vtbl,
        IDXGISwapChain_Vtbl, IDXGISwapChain1_Vtbl, IDXGISwapChain2_Vtbl, IDXGISwapChain3_Vtbl,
        IDXGISwapChain4_Vtbl,
    },
    core::IUnknown_Vtbl,
};

snowglass_hook::vtable! {
    [
        IDXGISwapChain_Vtbl,
        IDXGISwapChain1_Vtbl,
        IDXGISwapChain2_Vtbl,
        IDXGISwapChain3_Vtbl,
        IDXGISwapChain4_Vtbl,
    ],
    [
        IDXGIFactory_Vtbl,
        IDXGIFactory1_Vtbl,
        IDXGIFactory2_Vtbl,
        IDXGIFactory3_Vtbl,
        IDXGIFactory4_Vtbl,
        IDXGIFactory5_Vtbl,
        IDXGIFactory6_Vtbl,
        IDXGIFactory7_Vtbl,
    ],
}

pub(crate) type QueryInterfaceFn =
    unsafe extern "system" fn(*mut c_void, *const c_void, *mut *mut c_void) -> i32;
pub(crate) type ReleaseFn = unsafe extern "system" fn(*mut c_void) -> u32;
pub(crate) type PresentFn = unsafe extern "system" fn(*mut c_void, u32, u32) -> i32;
pub(crate) type Present1Fn = unsafe extern "system" fn(*mut c_void, u32, u32, *const c_void) -> i32;
pub(crate) type ResizeBuffersFn =
    unsafe extern "system" fn(*mut c_void, u32, u32, u32, u32, u32) -> i32;
pub(crate) type FactoryCreateSwapchainFn =
    unsafe extern "system" fn(*mut c_void, *mut c_void, *const c_void, *mut *mut c_void) -> i32;
pub(crate) type FactoryCreateSwapchainForHwndFn = unsafe extern "system" fn(
    *mut c_void,
    *mut c_void,
    *mut c_void,
    *const c_void,
    *const c_void,
    *mut c_void,
    *mut *mut c_void,
) -> i32;
pub(crate) type FactoryCreateSwapchainForCoreWindowFn = unsafe extern "system" fn(
    *mut c_void,
    *mut c_void,
    *mut c_void,
    *const c_void,
    *mut c_void,
    *mut *mut c_void,
) -> i32;
pub(crate) type FactoryCreateSwapchainForCompositionFn = unsafe extern "system" fn(
    *mut c_void,
    *mut c_void,
    *const c_void,
    *mut c_void,
    *mut *mut c_void,
) -> i32;

/// The layout-erased view of a hooked swapchain stored in the runtime
/// registry.
pub(crate) trait SwapchainHook: snowglass_hook::VtableHook {
    fn owner(&self) -> &std::sync::RwLock<Option<crate::runtime::SwapchainCallbacks>>;
    fn query_interface(&self) -> QueryInterfaceFn;
    fn release(&self) -> ReleaseFn;
    fn present(&self) -> PresentFn;
    fn resize_buffers(&self) -> ResizeBuffersFn;
    fn present1(&self) -> Option<Present1Fn>;
}

/// The layout-erased view of a hooked factory stored in the runtime registry.
pub(crate) trait FactoryHook: snowglass_hook::VtableHook {
    fn query_interface(&self) -> QueryInterfaceFn;
    fn release(&self) -> ReleaseFn;
    fn create_swapchain(&self) -> Option<FactoryCreateSwapchainFn>;
    fn create_swapchain_for_hwnd(&self) -> Option<FactoryCreateSwapchainForHwndFn>;
    fn create_swapchain_for_core_window(&self) -> Option<FactoryCreateSwapchainForCoreWindowFn>;
    fn create_swapchain_for_composition(&self) -> Option<FactoryCreateSwapchainForCompositionFn>;
}

snowglass_hook::com_vtable! {
    pub(crate) union IDXGISwapchainVtable for IDXGISwapChain4_Vtbl;
}

snowglass_hook::com_vtable! {
    /// Overlays every factory interface generation on one allocation, mirroring
    /// [`IDXGISwapchainVtable`].
    pub(crate) union IDXGIFactoryVtable for IDXGIFactory7_Vtbl;
}

snowglass_hook::impl_com_hook! {
    impl SwapchainHook for IDXGISwapchainHook {
        vtable IDXGISwapchainVtable;
        context {
            /// The backend that claimed this swapchain, assigned after
            /// installation once a registered claim accepts the creation
            /// device.
            owner: std::sync::RwLock<Option<crate::runtime::SwapchainCallbacks>>,
        };
        patch |patched| {
        // SAFETY: the slots written below were copied above, so each place
        // projection targets an initialized entry within the clone.
        unsafe {
            let unknown = patched.as_mut_ptr::<IUnknown_Vtbl>();
            (*unknown).QueryInterface =
                std::mem::transmute(crate::sys::query_interface as QueryInterfaceFn);
            (*unknown).Release = crate::sys::release;
            let table = patched.as_mut_ptr::<IDXGISwapChain_Vtbl>();
            (*table).Present = std::mem::transmute(crate::sys::present as PresentFn);
            (*table).ResizeBuffers =
                std::mem::transmute(crate::sys::resize_buffers as ResizeBuffersFn);
            if A::supports(std::any::TypeId::of::<IDXGISwapChain1_Vtbl>()) {
                let table1 = patched.as_mut_ptr::<IDXGISwapChain1_Vtbl>();
                (*table1).Present1 = std::mem::transmute(crate::sys::present1 as Present1Fn);
            }
        }
        }

        #[vtable = IUnknown_Vtbl]
        #[base = QueryInterface]
        fn query_interface(&self) -> QueryInterfaceFn;
        #[vtable = IUnknown_Vtbl]
        #[base = Release]
        fn release(&self) -> ReleaseFn;
        #[vtable = IDXGISwapChain_Vtbl]
        #[base = Present]
        fn present(&self) -> PresentFn;
        #[vtable = IDXGISwapChain_Vtbl]
        #[base = ResizeBuffers]
        fn resize_buffers(&self) -> ResizeBuffersFn;
        #[vtable = IDXGISwapChain1_Vtbl]
        #[base = Present1]
        fn present1(&self) -> Option<Present1Fn>;
    }
}

impl<A: Vtable> IDXGIFactoryHook<A> {
    /// # Safety
    ///
    /// `object` must be null or point to a live COM object whose vtable spans
    /// at least [`A::SLOTS`](Vtable::SLOTS) entries.
    pub(crate) unsafe fn clone_and_patch(object: *mut c_void) -> Option<Self> {
        if object.is_null() {
            return None;
        }
        let original = unsafe { *object.cast::<*const usize>() };
        unsafe { Self::clone_from(original) }
    }
}

snowglass_hook::impl_com_hook! {
    impl FactoryHook for IDXGIFactoryHook {
        vtable IDXGIFactoryVtable;
        patch |patched| {
        // SAFETY: the slots written below were copied above, so each place
        // projection targets an initialized entry within the clone.
        unsafe {
            let unknown = patched.as_mut_ptr::<IUnknown_Vtbl>();
            (*unknown).QueryInterface =
                std::mem::transmute(crate::sys::query_interface as QueryInterfaceFn);
            (*unknown).Release = crate::sys::factory_release;
            let table = patched.as_mut_ptr::<IDXGIFactory_Vtbl>();
            (*table).CreateSwapChain = std::mem::transmute(
                crate::sys::factory_create_swapchain as FactoryCreateSwapchainFn,
            );
            if A::supports(std::any::TypeId::of::<IDXGIFactory2_Vtbl>()) {
                let table2 = patched.as_mut_ptr::<IDXGIFactory2_Vtbl>();
                (*table2).CreateSwapChainForHwnd = std::mem::transmute(
                    crate::sys::factory_create_swapchain_for_hwnd
                        as FactoryCreateSwapchainForHwndFn,
                );
                (*table2).CreateSwapChainForCoreWindow = std::mem::transmute(
                    crate::sys::factory_create_swapchain_for_core_window
                        as FactoryCreateSwapchainForCoreWindowFn,
                );
                (*table2).CreateSwapChainForComposition = std::mem::transmute(
                    crate::sys::factory_create_swapchain_for_composition
                        as FactoryCreateSwapchainForCompositionFn,
                );
            }
        }
        }

        #[vtable = IUnknown_Vtbl]
        #[base = QueryInterface]
        fn query_interface(&self) -> QueryInterfaceFn;
        #[vtable = IUnknown_Vtbl]
        #[base = Release]
        fn release(&self) -> ReleaseFn;
        #[vtable = IDXGIFactory_Vtbl]
        #[base = CreateSwapChain]
        fn create_swapchain(&self) -> Option<FactoryCreateSwapchainFn>;
        #[vtable = IDXGIFactory2_Vtbl]
        #[base = CreateSwapChainForHwnd]
        fn create_swapchain_for_hwnd(&self) -> Option<FactoryCreateSwapchainForHwndFn>;
        #[vtable = IDXGIFactory2_Vtbl]
        #[base = CreateSwapChainForCoreWindow]
        fn create_swapchain_for_core_window(&self) -> Option<FactoryCreateSwapchainForCoreWindowFn>;
        #[vtable = IDXGIFactory2_Vtbl]
        #[base = CreateSwapChainForComposition]
        fn create_swapchain_for_composition(&self) -> Option<FactoryCreateSwapchainForCompositionFn>;
    }
}
