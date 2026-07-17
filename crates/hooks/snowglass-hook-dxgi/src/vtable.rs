use std::ffi::c_void;
use windows::{
    Win32::Graphics::Dxgi::{
        IDXGIFactory_Vtbl, IDXGIFactory1_Vtbl, IDXGIFactory2_Vtbl, IDXGIFactory3_Vtbl,
        IDXGIFactory4_Vtbl, IDXGIFactory5_Vtbl, IDXGIFactory6_Vtbl, IDXGIFactory7_Vtbl,
        IDXGISwapChain_Vtbl, IDXGISwapChain1_Vtbl,
    },
    core::IUnknown_Vtbl,
};

const SLOT_SIZE: usize = std::mem::size_of::<usize>();

pub(crate) const SWAPCHAIN_VTABLE_LEN: usize =
    std::mem::size_of::<IDXGISwapChain_Vtbl>() / SLOT_SIZE;
pub(crate) const SWAPCHAIN1_VTABLE_LEN: usize =
    std::mem::size_of::<IDXGISwapChain1_Vtbl>() / SLOT_SIZE;
pub(crate) const FACTORY_VTABLE_LEN: usize = std::mem::size_of::<IDXGIFactory_Vtbl>() / SLOT_SIZE;
pub(crate) const FACTORY1_VTABLE_LEN: usize = std::mem::size_of::<IDXGIFactory1_Vtbl>() / SLOT_SIZE;
pub(crate) const FACTORY2_VTABLE_LEN: usize = std::mem::size_of::<IDXGIFactory2_Vtbl>() / SLOT_SIZE;
pub(crate) const FACTORY3_VTABLE_LEN: usize = std::mem::size_of::<IDXGIFactory3_Vtbl>() / SLOT_SIZE;
pub(crate) const FACTORY4_VTABLE_LEN: usize = std::mem::size_of::<IDXGIFactory4_Vtbl>() / SLOT_SIZE;
pub(crate) const FACTORY5_VTABLE_LEN: usize = std::mem::size_of::<IDXGIFactory5_Vtbl>() / SLOT_SIZE;
pub(crate) const FACTORY6_VTABLE_LEN: usize = std::mem::size_of::<IDXGIFactory6_Vtbl>() / SLOT_SIZE;
pub(crate) const FACTORY7_VTABLE_LEN: usize = std::mem::size_of::<IDXGIFactory7_Vtbl>() / SLOT_SIZE;
pub(crate) const QUERY_INTERFACE_INDEX: usize =
    std::mem::offset_of!(IUnknown_Vtbl, QueryInterface) / SLOT_SIZE;
pub(crate) const RELEASE_INDEX: usize = std::mem::offset_of!(IUnknown_Vtbl, Release) / SLOT_SIZE;
pub(crate) const PRESENT_INDEX: usize =
    std::mem::offset_of!(IDXGISwapChain_Vtbl, Present) / SLOT_SIZE;
pub(crate) const RESIZE_BUFFERS_INDEX: usize =
    std::mem::offset_of!(IDXGISwapChain_Vtbl, ResizeBuffers) / SLOT_SIZE;
pub(crate) const PRESENT1_INDEX: usize =
    std::mem::offset_of!(IDXGISwapChain1_Vtbl, Present1) / SLOT_SIZE;
pub(crate) const FACTORY_CREATE_SWAPCHAIN_INDEX: usize =
    std::mem::offset_of!(IDXGIFactory_Vtbl, CreateSwapChain) / SLOT_SIZE;
pub(crate) const FACTORY_CREATE_SWAPCHAIN_FOR_HWND_INDEX: usize =
    std::mem::offset_of!(IDXGIFactory2_Vtbl, CreateSwapChainForHwnd) / SLOT_SIZE;
pub(crate) const FACTORY_CREATE_SWAPCHAIN_FOR_CORE_WINDOW_INDEX: usize =
    std::mem::offset_of!(IDXGIFactory2_Vtbl, CreateSwapChainForCoreWindow) / SLOT_SIZE;
pub(crate) const FACTORY_CREATE_SWAPCHAIN_FOR_COMPOSITION_INDEX: usize =
    std::mem::offset_of!(IDXGIFactory2_Vtbl, CreateSwapChainForComposition) / SLOT_SIZE;

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

pub(crate) struct SwapchainVtable {
    pub(crate) patched: Box<[usize]>,
    pub(crate) original: usize,
    pub(crate) query_interface: QueryInterfaceFn,
    pub(crate) release: ReleaseFn,
    pub(crate) present: PresentFn,
    pub(crate) present1: Option<Present1Fn>,
    pub(crate) resize_buffers: ResizeBuffersFn,
}

pub(crate) struct FactoryVtable {
    pub(crate) patched: Box<[usize]>,
    pub(crate) original: usize,
    pub(crate) query_interface: QueryInterfaceFn,
    pub(crate) create_swapchain: Option<FactoryCreateSwapchainFn>,
    pub(crate) create_swapchain_for_hwnd: Option<FactoryCreateSwapchainForHwndFn>,
    pub(crate) create_swapchain_for_core_window: Option<FactoryCreateSwapchainForCoreWindowFn>,
    pub(crate) create_swapchain_for_composition: Option<FactoryCreateSwapchainForCompositionFn>,
}

impl FactoryVtable {
    pub(crate) unsafe fn clone_and_patch_len(object: *mut c_void, length: usize) -> Option<Self> {
        if object.is_null() {
            return None;
        }
        let original = unsafe { *object.cast::<*const usize>() };
        if original.is_null() {
            return None;
        }
        unsafe { Self::clone_from(original, length) }
    }

    pub(crate) unsafe fn clone_from(original: *const usize, length: usize) -> Option<Self> {
        if original.is_null() || length < FACTORY_VTABLE_LEN {
            return None;
        }
        let factory2 = length >= FACTORY2_VTABLE_LEN;
        let mut patched = unsafe { std::slice::from_raw_parts(original, length) }
            .to_vec()
            .into_boxed_slice();
        let query_interface = unsafe {
            std::mem::transmute::<usize, QueryInterfaceFn>(patched[QUERY_INTERFACE_INDEX])
        };
        let create_swapchain = unsafe {
            std::mem::transmute::<usize, FactoryCreateSwapchainFn>(
                patched[FACTORY_CREATE_SWAPCHAIN_INDEX],
            )
        };
        patched[FACTORY_CREATE_SWAPCHAIN_INDEX] =
            crate::sys::factory_create_swapchain as *const () as usize;
        patched[QUERY_INTERFACE_INDEX] = crate::sys::query_interface as *const () as usize;
        let (
            create_swapchain_for_hwnd,
            create_swapchain_for_core_window,
            create_swapchain_for_composition,
        ) = if factory2 {
            let hwnd = unsafe {
                std::mem::transmute::<usize, FactoryCreateSwapchainForHwndFn>(
                    patched[FACTORY_CREATE_SWAPCHAIN_FOR_HWND_INDEX],
                )
            };
            let core_window = unsafe {
                std::mem::transmute::<usize, FactoryCreateSwapchainForCoreWindowFn>(
                    patched[FACTORY_CREATE_SWAPCHAIN_FOR_CORE_WINDOW_INDEX],
                )
            };
            let composition = unsafe {
                std::mem::transmute::<usize, FactoryCreateSwapchainForCompositionFn>(
                    patched[FACTORY_CREATE_SWAPCHAIN_FOR_COMPOSITION_INDEX],
                )
            };
            patched[FACTORY_CREATE_SWAPCHAIN_FOR_HWND_INDEX] =
                crate::sys::factory_create_swapchain_for_hwnd as *const () as usize;
            patched[FACTORY_CREATE_SWAPCHAIN_FOR_CORE_WINDOW_INDEX] =
                crate::sys::factory_create_swapchain_for_core_window as *const () as usize;
            patched[FACTORY_CREATE_SWAPCHAIN_FOR_COMPOSITION_INDEX] =
                crate::sys::factory_create_swapchain_for_composition as *const () as usize;
            (Some(hwnd), Some(core_window), Some(composition))
        } else {
            (None, None, None)
        };
        Some(Self {
            patched,
            original: original as usize,
            query_interface,
            create_swapchain: Some(create_swapchain),
            create_swapchain_for_hwnd,
            create_swapchain_for_core_window,
            create_swapchain_for_composition,
        })
    }
}

impl SwapchainVtable {
    pub(crate) unsafe fn clone_and_patch(object: *mut c_void) -> Option<Self> {
        if object.is_null() {
            return None;
        }
        let original = unsafe { *object.cast::<*const usize>() };
        if original.is_null() {
            return None;
        }
        unsafe { Self::clone_from(original, false) }
    }

    pub(crate) unsafe fn clone_and_patch_swapchain1(object: *mut c_void) -> Option<Self> {
        if object.is_null() {
            return None;
        }
        let original = unsafe { *object.cast::<*const usize>() };
        if original.is_null() {
            return None;
        }
        unsafe { Self::clone_from(original, true) }
    }

    pub(crate) unsafe fn clone_from(original: *const usize, swapchain1: bool) -> Option<Self> {
        if original.is_null() {
            return None;
        }
        let length = if swapchain1 {
            SWAPCHAIN1_VTABLE_LEN
        } else {
            SWAPCHAIN_VTABLE_LEN
        };
        let mut patched = unsafe { std::slice::from_raw_parts(original, length) }
            .to_vec()
            .into_boxed_slice();
        let query_interface = unsafe {
            std::mem::transmute::<usize, QueryInterfaceFn>(patched[QUERY_INTERFACE_INDEX])
        };
        let release = unsafe { std::mem::transmute::<usize, ReleaseFn>(patched[RELEASE_INDEX]) };
        let present = unsafe { std::mem::transmute::<usize, PresentFn>(patched[PRESENT_INDEX]) };
        let resize_buffers =
            unsafe { std::mem::transmute::<usize, ResizeBuffersFn>(patched[RESIZE_BUFFERS_INDEX]) };
        let present1 = swapchain1
            .then(|| unsafe { std::mem::transmute::<usize, Present1Fn>(patched[PRESENT1_INDEX]) });
        patched[QUERY_INTERFACE_INDEX] = crate::sys::query_interface as *const () as usize;
        patched[RELEASE_INDEX] = crate::sys::release as *const () as usize;
        patched[PRESENT_INDEX] = crate::sys::present as *const () as usize;
        patched[RESIZE_BUFFERS_INDEX] = crate::sys::resize_buffers as *const () as usize;
        if swapchain1 {
            patched[PRESENT1_INDEX] = crate::sys::present1 as *const () as usize;
        }
        Some(Self {
            patched,
            original: original as usize,
            query_interface,
            release,
            present,
            present1,
            resize_buffers,
        })
    }
}
