#![allow(clippy::missing_transmute_annotations)]

use std::any::TypeId;
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
    IDXGISwapChain_Vtbl,
    IDXGISwapChain1_Vtbl,
    IDXGISwapChain2_Vtbl,
    IDXGISwapChain3_Vtbl,
    IDXGISwapChain4_Vtbl,
    IDXGIFactory_Vtbl,
    IDXGIFactory1_Vtbl,
    IDXGIFactory2_Vtbl,
    IDXGIFactory3_Vtbl,
    IDXGIFactory4_Vtbl,
    IDXGIFactory5_Vtbl,
    IDXGIFactory6_Vtbl,
    IDXGIFactory7_Vtbl,
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
pub(crate) trait SwapchainHook: Send + Sync {
    /// The number of slots spanned by the cloned layout.
    fn slots(&self) -> usize;
    /// The [`Vtable`] layout the clone was created for.
    fn vtable_type(&self) -> TypeId;
    /// The vtable pointer that intercepts a hooked object.
    fn patched_vtable(&self) -> *const usize;
    /// The object's original vtable pointer.
    fn original_vtable(&self) -> *const usize;
    fn query_interface(&self) -> QueryInterfaceFn;
    fn release(&self) -> ReleaseFn;
    fn present(&self) -> PresentFn;
    fn resize_buffers(&self) -> ResizeBuffersFn;
    fn present1(&self) -> Option<Present1Fn>;
}

/// The layout-erased view of a hooked factory stored in the runtime registry.
pub(crate) trait FactoryHook: Send + Sync {
    /// The number of slots spanned by the cloned layout.
    fn slots(&self) -> usize;
    /// The [`Vtable`] layout the clone was created for.
    fn vtable_type(&self) -> TypeId;
    /// The vtable pointer that intercepts a hooked object.
    fn patched_vtable(&self) -> *const usize;
    /// The object's original vtable pointer.
    fn original_vtable(&self) -> *const usize;
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
    unsafe fn read(original: NonNull<usize>, slots: usize) -> Self {
        let mut cloned = Self {
            raw: [0; SWAPCHAIN_SLOT_CAPACITY],
        };
        unsafe {
            std::ptr::copy_nonoverlapping(
                original.as_ptr().cast_const(),
                (&raw mut cloned.raw).cast::<usize>(),
                slots,
            );
        }
        cloned
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
    unsafe fn read(original: NonNull<usize>, slots: usize) -> Self {
        let mut cloned = Self {
            raw: [0; FACTORY_SLOT_CAPACITY],
        };
        unsafe {
            std::ptr::copy_nonoverlapping(
                original.as_ptr().cast_const(),
                (&raw mut cloned.raw).cast::<usize>(),
                slots,
            );
        }
        cloned
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

pub(crate) struct IDXGISwapchainHook<A: Vtable> {
    /// Hook-patched clone installed as the object's vtable pointer.
    patched: Box<IDXGISwapchainVtable>,
    /// Verbatim copy of the original slots for downstream continuation.
    saved: IDXGISwapchainVtable,
    /// The object's original vtable pointer, restored across releases.
    original: NonNull<usize>,
    /// The widest interface generation cloned from the object's vtable.
    vtable: PhantomData<A>,
}

// SAFETY: `original` identifies an immutable vtable owned by the hooked
// module for the process lifetime; the hook only reads through it.
unsafe impl<A: Vtable> Send for IDXGISwapchainHook<A> {}
unsafe impl<A: Vtable> Sync for IDXGISwapchainHook<A> {}

impl<A: Vtable> IDXGISwapchainHook<A> {
    /// # Safety
    ///
    /// `original` must be null or point to a vtable spanning at least
    /// [`A::SLOTS`](Vtable::SLOTS) entries.
    pub(crate) unsafe fn clone_from(original: *const usize) -> Option<Self> {
        let original = NonNull::new(original.cast_mut())?;
        let saved = unsafe { IDXGISwapchainVtable::read(original, A::SLOTS) };
        let mut patched = Box::new(unsafe { IDXGISwapchainVtable::read(original, A::SLOTS) });
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
            if A::SLOTS >= <IDXGISwapChain1_Vtbl as Vtable>::SLOTS {
                let table1 = patched.table1_mut();
                (*table1).Present1 = std::mem::transmute(crate::sys::present1 as Present1Fn);
            }
        }
        Some(Self {
            patched,
            saved,
            original,
            vtable: PhantomData,
        })
    }
}

impl<A: Vtable> SwapchainHook for IDXGISwapchainHook<A> {
    fn slots(&self) -> usize {
        A::SLOTS
    }

    fn vtable_type(&self) -> TypeId {
        TypeId::of::<A>()
    }

    fn patched_vtable(&self) -> *const usize {
        std::ptr::from_ref::<IDXGISwapchainVtable>(&self.patched).cast()
    }

    fn original_vtable(&self) -> *const usize {
        self.original.as_ptr()
    }

    fn query_interface(&self) -> QueryInterfaceFn {
        unsafe { std::mem::transmute((*self.saved.unknown()).QueryInterface) }
    }

    fn release(&self) -> ReleaseFn {
        unsafe { (*self.saved.unknown()).Release }
    }

    fn present(&self) -> PresentFn {
        unsafe { std::mem::transmute((*self.saved.table()).Present) }
    }

    fn resize_buffers(&self) -> ResizeBuffersFn {
        unsafe { std::mem::transmute((*self.saved.table()).ResizeBuffers) }
    }

    fn present1(&self) -> Option<Present1Fn> {
        (A::SLOTS >= <IDXGISwapChain1_Vtbl as Vtable>::SLOTS)
            .then(|| unsafe { std::mem::transmute((*self.saved.table1()).Present1) })
    }
}

pub(crate) struct IDXGIFactoryHook<A: Vtable> {
    /// Hook-patched clone installed as the object's vtable pointer.
    patched: Box<IDXGIFactoryVtable>,
    /// Verbatim copy of the original slots for downstream continuation.
    saved: IDXGIFactoryVtable,
    /// The object's original vtable pointer, restored across releases.
    original: NonNull<usize>,
    /// The widest interface generation cloned from the object's vtable.
    vtable: PhantomData<A>,
}

// SAFETY: see `IDXGISwapchainHook`.
unsafe impl<A: Vtable> Send for IDXGIFactoryHook<A> {}
unsafe impl<A: Vtable> Sync for IDXGIFactoryHook<A> {}

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

    /// # Safety
    ///
    /// `original` must be null or point to a vtable spanning at least
    /// [`A::SLOTS`](Vtable::SLOTS) entries.
    pub(crate) unsafe fn clone_from(original: *const usize) -> Option<Self> {
        let original = NonNull::new(original.cast_mut())?;
        let saved = unsafe { IDXGIFactoryVtable::read(original, A::SLOTS) };
        let mut patched = Box::new(unsafe { IDXGIFactoryVtable::read(original, A::SLOTS) });
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
            if A::SLOTS >= <IDXGIFactory2_Vtbl as Vtable>::SLOTS {
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
            original,
            vtable: PhantomData,
        })
    }
}

impl<A: Vtable> FactoryHook for IDXGIFactoryHook<A> {
    fn slots(&self) -> usize {
        A::SLOTS
    }

    fn vtable_type(&self) -> TypeId {
        TypeId::of::<A>()
    }

    fn patched_vtable(&self) -> *const usize {
        std::ptr::from_ref::<IDXGIFactoryVtable>(&self.patched).cast()
    }

    fn original_vtable(&self) -> *const usize {
        self.original.as_ptr()
    }

    fn query_interface(&self) -> QueryInterfaceFn {
        unsafe { std::mem::transmute((*self.saved.unknown()).QueryInterface) }
    }

    fn release(&self) -> ReleaseFn {
        unsafe { (*self.saved.unknown()).Release }
    }

    fn create_swapchain(&self) -> Option<FactoryCreateSwapchainFn> {
        Some(unsafe { std::mem::transmute((*self.saved.table()).CreateSwapChain) })
    }

    fn create_swapchain_for_hwnd(&self) -> Option<FactoryCreateSwapchainForHwndFn> {
        (A::SLOTS >= <IDXGIFactory2_Vtbl as Vtable>::SLOTS)
            .then(|| unsafe { std::mem::transmute((*self.saved.table2()).CreateSwapChainForHwnd) })
    }

    fn create_swapchain_for_core_window(&self) -> Option<FactoryCreateSwapchainForCoreWindowFn> {
        (A::SLOTS >= <IDXGIFactory2_Vtbl as Vtable>::SLOTS).then(|| unsafe {
            std::mem::transmute((*self.saved.table2()).CreateSwapChainForCoreWindow)
        })
    }

    fn create_swapchain_for_composition(&self) -> Option<FactoryCreateSwapchainForCompositionFn> {
        (A::SLOTS >= <IDXGIFactory2_Vtbl as Vtable>::SLOTS).then(|| unsafe {
            std::mem::transmute((*self.saved.table2()).CreateSwapChainForComposition)
        })
    }
}
