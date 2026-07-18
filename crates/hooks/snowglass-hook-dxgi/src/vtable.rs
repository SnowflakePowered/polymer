#![allow(clippy::missing_transmute_annotations)]

use std::ffi::c_void;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ptr::NonNull;
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

const SLOT_SIZE: usize = size_of::<usize>();
const SWAPCHAIN_SLOT_CAPACITY: usize = <IDXGISwapChain4_Vtbl as Vtable>::SLOTS;
const FACTORY_SLOT_CAPACITY: usize = <IDXGIFactory7_Vtbl as Vtable>::SLOTS;

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

#[repr(C)]
pub(crate) union IDXGISwapchainVtable {
    raw: [usize; SWAPCHAIN_SLOT_CAPACITY],
    swapchain: ManuallyDrop<IDXGISwapChain_Vtbl>,
    swapchain1: ManuallyDrop<IDXGISwapChain1_Vtbl>,
    swapchain2: ManuallyDrop<IDXGISwapChain2_Vtbl>,
    swapchain3: ManuallyDrop<IDXGISwapChain3_Vtbl>,
    swapchain4: ManuallyDrop<IDXGISwapChain4_Vtbl>,
}

const _: () = assert!(size_of::<IDXGISwapchainVtable>() == SWAPCHAIN_SLOT_CAPACITY * SLOT_SIZE);

impl IDXGISwapchainVtable {
    /// # Safety
    ///
    /// `original` must point to a vtable spanning at least `slots` entries.
    unsafe fn read(original: NonNull<c_void>, slots: usize) -> Self {
        let mut cloned = Self {
            raw: [0; SWAPCHAIN_SLOT_CAPACITY],
        };
        unsafe {
            std::ptr::copy_nonoverlapping(
                original.as_ptr().cast::<usize>().cast_const(),
                (&raw mut cloned.raw).cast::<usize>(),
                slots,
            );
        }
        cloned
    }

    fn unknown_mut(&mut self) -> *mut IUnknown_Vtbl {
        (&raw mut self.swapchain).cast()
    }

    fn table_mut(&mut self) -> *mut IDXGISwapChain_Vtbl {
        (&raw mut self.swapchain).cast()
    }

    fn table1_mut(&mut self) -> *mut IDXGISwapChain1_Vtbl {
        (&raw mut self.swapchain1).cast()
    }
}

/// Overlays every factory interface generation on one allocation, mirroring
/// [`IDXGISwapchainVtable`].
#[repr(C)]
pub(crate) union IDXGIFactoryVtable {
    raw: [usize; FACTORY_SLOT_CAPACITY],
    factory: ManuallyDrop<IDXGIFactory_Vtbl>,
    factory1: ManuallyDrop<IDXGIFactory1_Vtbl>,
    factory2: ManuallyDrop<IDXGIFactory2_Vtbl>,
    factory3: ManuallyDrop<IDXGIFactory3_Vtbl>,
    factory4: ManuallyDrop<IDXGIFactory4_Vtbl>,
    factory5: ManuallyDrop<IDXGIFactory5_Vtbl>,
    factory6: ManuallyDrop<IDXGIFactory6_Vtbl>,
    factory7: ManuallyDrop<IDXGIFactory7_Vtbl>,
}

const _: () = assert!(size_of::<IDXGIFactoryVtable>() == FACTORY_SLOT_CAPACITY * SLOT_SIZE);

impl IDXGIFactoryVtable {
    /// # Safety
    ///
    /// `original` must point to a vtable spanning at least `slots` entries.
    unsafe fn read(original: NonNull<c_void>, slots: usize) -> Self {
        let mut cloned = Self {
            raw: [0; FACTORY_SLOT_CAPACITY],
        };
        unsafe {
            std::ptr::copy_nonoverlapping(
                original.as_ptr().cast::<usize>().cast_const(),
                (&raw mut cloned.raw).cast::<usize>(),
                slots,
            );
        }
        cloned
    }

    fn unknown_mut(&mut self) -> *mut IUnknown_Vtbl {
        (&raw mut self.factory).cast()
    }

    fn table_mut(&mut self) -> *mut IDXGIFactory_Vtbl {
        (&raw mut self.factory).cast()
    }

    fn table2_mut(&mut self) -> *mut IDXGIFactory2_Vtbl {
        (&raw mut self.factory2).cast()
    }
}

pub(crate) struct IDXGISwapchainHook<A: Vtable> {
    /// Hook-patched clone installed as the object's vtable pointer.
    patched: Box<IDXGISwapchainVtable>,
    /// Verbatim copy of the original slots for downstream continuation.
    saved: IDXGISwapchainVtable,
    /// The object's original vtable pointer, restored across releases.
    original: NonNull<c_void>,
    /// The widest interface generation cloned from the object's vtable.
    vtable: PhantomData<A>,
}

snowglass_hook::impl_com_hook! {
    impl SwapchainHook for IDXGISwapchainHook {
        vtable IDXGISwapchainVtable;
        patch |patched| {
        // SAFETY: the slots written below were copied above, so each place
        // projection targets an initialized entry within the clone.
        unsafe {
            let unknown = patched.unknown_mut();
            (*unknown).QueryInterface =
                std::mem::transmute(crate::sys::query_interface as QueryInterfaceFn);
            (*unknown).Release = crate::sys::release;
            let table = patched.table_mut();
            (*table).Present = std::mem::transmute(crate::sys::present as PresentFn);
            (*table).ResizeBuffers =
                std::mem::transmute(crate::sys::resize_buffers as ResizeBuffersFn);
            if A::supports(std::any::TypeId::of::<IDXGISwapChain1_Vtbl>()) {
                let table1 = patched.table1_mut();
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

pub(crate) struct IDXGIFactoryHook<A: Vtable> {
    /// Hook-patched clone installed as the object's vtable pointer.
    patched: Box<IDXGIFactoryVtable>,
    /// Verbatim copy of the original slots for downstream continuation.
    saved: IDXGIFactoryVtable,
    /// The object's original vtable pointer, restored across releases.
    original: NonNull<c_void>,
    /// The widest interface generation cloned from the object's vtable.
    vtable: PhantomData<A>,
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
            let unknown = patched.unknown_mut();
            (*unknown).QueryInterface =
                std::mem::transmute(crate::sys::query_interface as QueryInterfaceFn);
            (*unknown).Release = crate::sys::factory_release;
            let table = patched.table_mut();
            (*table).CreateSwapChain = std::mem::transmute(
                crate::sys::factory_create_swapchain as FactoryCreateSwapchainFn,
            );
            if A::supports(std::any::TypeId::of::<IDXGIFactory2_Vtbl>()) {
                let table2 = patched.table2_mut();
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
