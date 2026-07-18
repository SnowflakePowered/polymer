#![allow(clippy::missing_transmute_annotations)]

use std::ffi::c_void;
use std::mem::ManuallyDrop;
use windows::{
    Win32::Graphics::Dxgi::{
        IDXGIFactory_Vtbl, IDXGIFactory1_Vtbl, IDXGIFactory2_Vtbl, IDXGIFactory3_Vtbl,
        IDXGIFactory4_Vtbl, IDXGIFactory5_Vtbl, IDXGIFactory6_Vtbl, IDXGIFactory7_Vtbl,
        IDXGISwapChain_Vtbl, IDXGISwapChain1_Vtbl, IDXGISwapChain2_Vtbl, IDXGISwapChain3_Vtbl,
        IDXGISwapChain4_Vtbl,
    },
    core::IUnknown_Vtbl,
};

const SLOT_SIZE: usize = size_of::<usize>();
const SWAPCHAIN_SLOT_CAPACITY: usize = size_of::<IDXGISwapChain4_Vtbl>() / SLOT_SIZE;
const FACTORY_SLOT_CAPACITY: usize = size_of::<IDXGIFactory7_Vtbl>() / SLOT_SIZE;

/// The widest swapchain interface generation cloned from an object's vtable.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SwapchainKind {
    Swapchain,
    Swapchain1,
    Swapchain2,
    Swapchain3,
    Swapchain4,
}

impl SwapchainKind {
    pub(crate) const fn slots(self) -> usize {
        (match self {
            Self::Swapchain => size_of::<IDXGISwapChain_Vtbl>(),
            Self::Swapchain1 => size_of::<IDXGISwapChain1_Vtbl>(),
            Self::Swapchain2 => size_of::<IDXGISwapChain2_Vtbl>(),
            Self::Swapchain3 => size_of::<IDXGISwapChain3_Vtbl>(),
            Self::Swapchain4 => size_of::<IDXGISwapChain4_Vtbl>(),
        }) / SLOT_SIZE
    }
}

/// The widest factory interface generation cloned from an object's vtable.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum FactoryKind {
    Factory,
    Factory1,
    Factory2,
    Factory3,
    Factory4,
    Factory5,
    Factory6,
    Factory7,
}

impl FactoryKind {
    pub(crate) const fn slots(self) -> usize {
        (match self {
            Self::Factory => size_of::<IDXGIFactory_Vtbl>(),
            Self::Factory1 => size_of::<IDXGIFactory1_Vtbl>(),
            Self::Factory2 => size_of::<IDXGIFactory2_Vtbl>(),
            Self::Factory3 => size_of::<IDXGIFactory3_Vtbl>(),
            Self::Factory4 => size_of::<IDXGIFactory4_Vtbl>(),
            Self::Factory5 => size_of::<IDXGIFactory5_Vtbl>(),
            Self::Factory6 => size_of::<IDXGIFactory6_Vtbl>(),
            Self::Factory7 => size_of::<IDXGIFactory7_Vtbl>(),
        }) / SLOT_SIZE
    }
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
    /// `original` must point to a vtable spanning at least `kind.slots()`
    /// entries.
    unsafe fn read(original: *const usize, kind: SwapchainKind) -> Self {
        let mut slots = Self {
            raw: [0; SWAPCHAIN_SLOT_CAPACITY],
        };
        unsafe {
            std::ptr::copy_nonoverlapping(
                original,
                (&raw mut slots.raw).cast::<usize>(),
                kind.slots(),
            );
        }
        slots
    }

    fn unknown(&self) -> *const IUnknown_Vtbl {
        (&raw const self.swapchain).cast()
    }

    fn table(&self) -> *const IDXGISwapChain_Vtbl {
        (&raw const self.swapchain).cast()
    }

    fn table1(&self) -> *const IDXGISwapChain1_Vtbl {
        (&raw const self.swapchain1).cast()
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
/// [`SwapchainSlots`].
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
    /// `original` must point to a vtable spanning at least `kind.slots()`
    /// entries.
    unsafe fn read(original: *const usize, kind: FactoryKind) -> Self {
        let mut slots = Self {
            raw: [0; FACTORY_SLOT_CAPACITY],
        };
        unsafe {
            std::ptr::copy_nonoverlapping(
                original,
                (&raw mut slots.raw).cast::<usize>(),
                kind.slots(),
            );
        }
        slots
    }

    fn unknown(&self) -> *const IUnknown_Vtbl {
        (&raw const self.factory).cast()
    }

    fn table(&self) -> *const IDXGIFactory_Vtbl {
        (&raw const self.factory).cast()
    }

    fn table2(&self) -> *const IDXGIFactory2_Vtbl {
        (&raw const self.factory2).cast()
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

pub(crate) struct IDXGISwapchainHook {
    /// Hook-patched clone installed as the object's vtable pointer.
    patched: Box<IDXGISwapchainVtable>,
    /// Verbatim copy of the original slots for downstream continuation.
    saved: IDXGISwapchainVtable,
    /// The object's original vtable pointer, restored across releases.
    original: usize,
    kind: SwapchainKind,
}

impl IDXGISwapchainHook {
    /// # Safety
    ///
    /// `original` must be null or point to a vtable spanning at least
    /// `kind.slots()` entries.
    pub(crate) unsafe fn clone_from(original: *const usize, kind: SwapchainKind) -> Option<Self> {
        if original.is_null() {
            return None;
        }
        let saved = unsafe { IDXGISwapchainVtable::read(original, kind) };
        let mut patched = Box::new(unsafe { IDXGISwapchainVtable::read(original, kind) });
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
            if kind >= SwapchainKind::Swapchain1 {
                let table1 = patched.table1_mut();
                (*table1).Present1 = std::mem::transmute(crate::sys::present1 as Present1Fn);
            }
        }
        Some(Self {
            patched,
            saved,
            original: original as usize,
            kind,
        })
    }

    pub(crate) fn kind(&self) -> SwapchainKind {
        self.kind
    }

    /// The vtable pointer that intercepts a hooked object.
    pub(crate) fn patched_vtable(&self) -> *const usize {
        std::ptr::from_ref::<IDXGISwapchainVtable>(&self.patched).cast()
    }

    /// The object's original vtable pointer.
    pub(crate) fn original_vtable(&self) -> *const usize {
        self.original as *const usize
    }

    pub(crate) fn query_interface(&self) -> QueryInterfaceFn {
        unsafe { std::mem::transmute((*self.saved.unknown()).QueryInterface) }
    }

    pub(crate) fn release(&self) -> ReleaseFn {
        unsafe { (*self.saved.unknown()).Release }
    }

    pub(crate) fn present(&self) -> PresentFn {
        unsafe { std::mem::transmute((*self.saved.table()).Present) }
    }

    pub(crate) fn resize_buffers(&self) -> ResizeBuffersFn {
        unsafe { std::mem::transmute((*self.saved.table()).ResizeBuffers) }
    }

    pub(crate) fn present1(&self) -> Option<Present1Fn> {
        (self.kind >= SwapchainKind::Swapchain1)
            .then(|| unsafe { std::mem::transmute((*self.saved.table1()).Present1) })
    }
}

pub(crate) struct IDXGIFactoryHook {
    /// Hook-patched clone installed as the object's vtable pointer.
    patched: Box<IDXGIFactoryVtable>,
    /// Verbatim copy of the original slots for downstream continuation.
    saved: IDXGIFactoryVtable,
    /// The object's original vtable pointer, restored across releases.
    original: usize,
    kind: FactoryKind,
}

impl IDXGIFactoryHook {
    /// # Safety
    ///
    /// `object` must be null or point to a live COM object supporting `kind`.
    pub(crate) unsafe fn clone_and_patch(object: *mut c_void, kind: FactoryKind) -> Option<Self> {
        if object.is_null() {
            return None;
        }
        let original = unsafe { *object.cast::<*const usize>() };
        unsafe { Self::clone_from(original, kind) }
    }

    /// # Safety
    ///
    /// `original` must be null or point to a vtable spanning at least
    /// `kind.slots()` entries.
    pub(crate) unsafe fn clone_from(original: *const usize, kind: FactoryKind) -> Option<Self> {
        if original.is_null() {
            return None;
        }
        let saved = unsafe { IDXGIFactoryVtable::read(original, kind) };
        let mut patched = Box::new(unsafe { IDXGIFactoryVtable::read(original, kind) });
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
            if kind >= FactoryKind::Factory2 {
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
        Some(Self {
            patched,
            saved,
            original: original as usize,
            kind,
        })
    }

    pub(crate) fn kind(&self) -> FactoryKind {
        self.kind
    }

    /// The vtable pointer that intercepts a hooked object.
    pub(crate) fn patched_vtable(&self) -> *const usize {
        std::ptr::from_ref::<IDXGIFactoryVtable>(&self.patched).cast()
    }

    /// The object's original vtable pointer.
    pub(crate) fn original_vtable(&self) -> *const usize {
        self.original as *const usize
    }

    pub(crate) fn query_interface(&self) -> QueryInterfaceFn {
        unsafe { std::mem::transmute((*self.saved.unknown()).QueryInterface) }
    }

    pub(crate) fn release(&self) -> ReleaseFn {
        unsafe { (*self.saved.unknown()).Release }
    }

    pub(crate) fn create_swapchain(&self) -> Option<FactoryCreateSwapchainFn> {
        Some(unsafe { std::mem::transmute((*self.saved.table()).CreateSwapChain) })
    }

    pub(crate) fn create_swapchain_for_hwnd(&self) -> Option<FactoryCreateSwapchainForHwndFn> {
        (self.kind >= FactoryKind::Factory2)
            .then(|| unsafe { std::mem::transmute((*self.saved.table2()).CreateSwapChainForHwnd) })
    }

    pub(crate) fn create_swapchain_for_core_window(
        &self,
    ) -> Option<FactoryCreateSwapchainForCoreWindowFn> {
        (self.kind >= FactoryKind::Factory2).then(|| unsafe {
            std::mem::transmute((*self.saved.table2()).CreateSwapChainForCoreWindow)
        })
    }

    pub(crate) fn create_swapchain_for_composition(
        &self,
    ) -> Option<FactoryCreateSwapchainForCompositionFn> {
        (self.kind >= FactoryKind::Factory2).then(|| unsafe {
            std::mem::transmute((*self.saved.table2()).CreateSwapChainForComposition)
        })
    }
}
