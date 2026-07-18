//! Typed clones of D3D12 device and graphics command-list vtables with
//! interception applied.
//!
//! Each hooked object receives a heap clone of its vtable with the tracked
//! slots redirected. The clone is copied as opaque words (a vtable may legally
//! carry null or reserved slots, so whole-struct reads would assert validity
//! the source cannot guarantee), while individual slots are read and patched
//! through the `windows` crate's vtable definitions overlaid by a union.

use std::any::TypeId;
use std::ffi::c_void;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ptr::NonNull;
use windows::{
    Win32::{
        Foundation::RECT,
        Graphics::Direct3D12::{
            D3D12_BARRIER_GROUP, D3D12_COMMAND_LIST_FLAGS, D3D12_COMMAND_LIST_TYPE,
            D3D12_CPU_DESCRIPTOR_HANDLE, D3D12_DESCRIPTOR_HEAP_DESC, D3D12_DESCRIPTOR_HEAP_TYPE,
            D3D12_GPU_DESCRIPTOR_HANDLE, D3D12_RENDER_TARGET_VIEW_DESC, D3D12_RESOURCE_BARRIER,
            D3D12_SHADER_RESOURCE_VIEW_DESC, D3D12_VIEWPORT, ID3D12Device_Vtbl, ID3D12Device1_Vtbl,
            ID3D12Device2_Vtbl, ID3D12Device3_Vtbl, ID3D12Device4_Vtbl, ID3D12Device5_Vtbl,
            ID3D12Device6_Vtbl, ID3D12Device7_Vtbl, ID3D12Device8_Vtbl, ID3D12Device9_Vtbl,
            ID3D12Device10_Vtbl, ID3D12Device11_Vtbl, ID3D12Device12_Vtbl, ID3D12Device13_Vtbl,
            ID3D12Device14_Vtbl, ID3D12GraphicsCommandList_Vtbl, ID3D12GraphicsCommandList1_Vtbl,
            ID3D12GraphicsCommandList2_Vtbl, ID3D12GraphicsCommandList3_Vtbl,
            ID3D12GraphicsCommandList4_Vtbl, ID3D12GraphicsCommandList5_Vtbl,
            ID3D12GraphicsCommandList6_Vtbl, ID3D12GraphicsCommandList7_Vtbl,
            ID3D12GraphicsCommandList8_Vtbl, ID3D12GraphicsCommandList9_Vtbl,
            ID3D12GraphicsCommandList10_Vtbl,
        },
    },
    core::{BOOL, GUID, HRESULT, IUnknown_Vtbl},
};

snowglass_hook::vtable! {
    ID3D12GraphicsCommandList_Vtbl,
    ID3D12GraphicsCommandList1_Vtbl,
    ID3D12GraphicsCommandList2_Vtbl,
    ID3D12GraphicsCommandList3_Vtbl,
    ID3D12GraphicsCommandList4_Vtbl,
    ID3D12GraphicsCommandList5_Vtbl,
    ID3D12GraphicsCommandList6_Vtbl,
    ID3D12GraphicsCommandList7_Vtbl,
    ID3D12GraphicsCommandList8_Vtbl,
    ID3D12GraphicsCommandList9_Vtbl,
    ID3D12GraphicsCommandList10_Vtbl,
    ID3D12Device_Vtbl,
    ID3D12Device1_Vtbl,
    ID3D12Device2_Vtbl,
    ID3D12Device3_Vtbl,
    ID3D12Device4_Vtbl,
    ID3D12Device5_Vtbl,
    ID3D12Device6_Vtbl,
    ID3D12Device7_Vtbl,
    ID3D12Device8_Vtbl,
    ID3D12Device9_Vtbl,
    ID3D12Device10_Vtbl,
    ID3D12Device11_Vtbl,
    ID3D12Device12_Vtbl,
    ID3D12Device13_Vtbl,
    ID3D12Device14_Vtbl,
}

const SLOT_SIZE: usize = size_of::<usize>();
const LIST_SLOT_CAPACITY: usize = <ID3D12GraphicsCommandList10_Vtbl as Vtable>::SLOTS;
const DEVICE_SLOT_CAPACITY: usize = <ID3D12Device14_Vtbl as Vtable>::SLOTS;

pub(crate) type ReleaseFn = unsafe extern "system" fn(*mut c_void) -> u32;
pub(crate) type ResetFn =
    unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> HRESULT;
pub(crate) type SetViewportsFn = unsafe extern "system" fn(*mut c_void, u32, *const D3D12_VIEWPORT);
pub(crate) type SetScissorsFn = unsafe extern "system" fn(*mut c_void, u32, *const RECT);
pub(crate) type DrawInstancedFn = unsafe extern "system" fn(*mut c_void, u32, u32, u32, u32);
pub(crate) type DrawIndexedInstancedFn =
    unsafe extern "system" fn(*mut c_void, u32, u32, u32, i32, u32);
pub(crate) type ResourceBarrierFn =
    unsafe extern "system" fn(*mut c_void, u32, *const D3D12_RESOURCE_BARRIER);
pub(crate) type SetGraphicsRootDescriptorTableFn =
    unsafe extern "system" fn(*mut c_void, u32, D3D12_GPU_DESCRIPTOR_HANDLE);
pub(crate) type SetGraphicsRootSignatureFn = unsafe extern "system" fn(*mut c_void, *mut c_void);
pub(crate) type OmSetRenderTargetsFn = unsafe extern "system" fn(
    *mut c_void,
    u32,
    *const D3D12_CPU_DESCRIPTOR_HANDLE,
    BOOL,
    *const D3D12_CPU_DESCRIPTOR_HANDLE,
);
pub(crate) type EnhancedBarrierFn =
    unsafe extern "system" fn(*mut c_void, u32, *const D3D12_BARRIER_GROUP);
pub(crate) type CreateCommandListFn = unsafe extern "system" fn(
    *mut c_void,
    u32,
    D3D12_COMMAND_LIST_TYPE,
    *mut c_void,
    *mut c_void,
    *const GUID,
    *mut *mut c_void,
) -> HRESULT;
pub(crate) type CreateCommandList1Fn = unsafe extern "system" fn(
    *mut c_void,
    u32,
    D3D12_COMMAND_LIST_TYPE,
    D3D12_COMMAND_LIST_FLAGS,
    *const GUID,
    *mut *mut c_void,
) -> HRESULT;
pub(crate) type CreateDescriptorHeapFn = unsafe extern "system" fn(
    *mut c_void,
    *const D3D12_DESCRIPTOR_HEAP_DESC,
    *const GUID,
    *mut *mut c_void,
) -> HRESULT;
pub(crate) type CreateShaderResourceViewFn = unsafe extern "system" fn(
    *mut c_void,
    *mut c_void,
    *const D3D12_SHADER_RESOURCE_VIEW_DESC,
    D3D12_CPU_DESCRIPTOR_HANDLE,
);
pub(crate) type CreateRenderTargetViewFn = unsafe extern "system" fn(
    *mut c_void,
    *mut c_void,
    *const D3D12_RENDER_TARGET_VIEW_DESC,
    D3D12_CPU_DESCRIPTOR_HANDLE,
);
pub(crate) type CopyDescriptorsSimpleFn = unsafe extern "system" fn(
    *mut c_void,
    u32,
    D3D12_CPU_DESCRIPTOR_HANDLE,
    D3D12_CPU_DESCRIPTOR_HANDLE,
    D3D12_DESCRIPTOR_HEAP_TYPE,
);

/// The layout-erased view of a hooked graphics command list stored in the
/// tracking registry.
pub(crate) trait CommandListHook: Send + Sync {
    /// The number of slots spanned by the cloned layout.
    fn slots(&self) -> usize;
    /// The [`Vtable`] layout the clone was created for.
    fn vtable_type(&self) -> TypeId;
    /// The vtable pointer that intercepts a hooked object.
    fn patched_vtable(&self) -> *const usize;
    /// The object's original vtable pointer.
    fn original_vtable(&self) -> *const usize;
    fn release(&self) -> ReleaseFn;
    fn reset(&self) -> ResetFn;
    fn set_viewports(&self) -> SetViewportsFn;
    fn set_scissors(&self) -> SetScissorsFn;
    fn draw_instanced(&self) -> DrawInstancedFn;
    fn draw_indexed_instanced(&self) -> DrawIndexedInstancedFn;
    fn resource_barrier(&self) -> ResourceBarrierFn;
    fn set_graphics_root_descriptor_table(&self) -> SetGraphicsRootDescriptorTableFn;
    fn set_graphics_root_signature(&self) -> SetGraphicsRootSignatureFn;
    fn om_set_render_targets(&self) -> OmSetRenderTargetsFn;
    fn barrier(&self) -> Option<EnhancedBarrierFn>;
}

/// The layout-erased view of a hooked device stored in the tracking registry.
pub(crate) trait DeviceHook: Send + Sync {
    /// The number of slots spanned by the cloned layout.
    fn slots(&self) -> usize;
    /// The [`Vtable`] layout the clone was created for.
    fn vtable_type(&self) -> TypeId;
    /// The vtable pointer that intercepts a hooked object.
    fn patched_vtable(&self) -> *const usize;
    /// The object's original vtable pointer.
    fn original_vtable(&self) -> *const usize;
    fn release(&self) -> ReleaseFn;
    fn create_command_list(&self) -> CreateCommandListFn;
    fn create_command_list1(&self) -> Option<CreateCommandList1Fn>;
    fn create_descriptor_heap(&self) -> CreateDescriptorHeapFn;
    fn create_shader_resource_view(&self) -> CreateShaderResourceViewFn;
    fn create_render_target_view(&self) -> CreateRenderTargetViewFn;
    fn copy_descriptors_simple(&self) -> CopyDescriptorsSimpleFn;
}

/// Overlays every graphics command-list interface generation on one
/// allocation. The generations extend one another, so a narrower interface is
/// a prefix of a wider one; `raw` spans the widest layout and is the untyped
/// view used for copying.
#[repr(C)]
pub(crate) union ID3D12GraphicsCommandListVtable {
    raw: [usize; LIST_SLOT_CAPACITY],
    list: ManuallyDrop<ID3D12GraphicsCommandList_Vtbl>,
    list1: ManuallyDrop<ID3D12GraphicsCommandList1_Vtbl>,
    list2: ManuallyDrop<ID3D12GraphicsCommandList2_Vtbl>,
    list3: ManuallyDrop<ID3D12GraphicsCommandList3_Vtbl>,
    list4: ManuallyDrop<ID3D12GraphicsCommandList4_Vtbl>,
    list5: ManuallyDrop<ID3D12GraphicsCommandList5_Vtbl>,
    list6: ManuallyDrop<ID3D12GraphicsCommandList6_Vtbl>,
    list7: ManuallyDrop<ID3D12GraphicsCommandList7_Vtbl>,
    list8: ManuallyDrop<ID3D12GraphicsCommandList8_Vtbl>,
    list9: ManuallyDrop<ID3D12GraphicsCommandList9_Vtbl>,
    list10: ManuallyDrop<ID3D12GraphicsCommandList10_Vtbl>,
}

const _: () =
    assert!(size_of::<ID3D12GraphicsCommandListVtable>() == LIST_SLOT_CAPACITY * SLOT_SIZE);

impl ID3D12GraphicsCommandListVtable {
    /// # Safety
    ///
    /// `original` must point to a vtable spanning at least `slots` entries.
    unsafe fn read(original: NonNull<usize>, slots: usize) -> Self {
        let mut cloned = Self {
            raw: [0; LIST_SLOT_CAPACITY],
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
        (&raw const self.list).cast()
    }

    fn table(&self) -> *const ID3D12GraphicsCommandList_Vtbl {
        (&raw const self.list).cast()
    }

    fn table7(&self) -> *const ID3D12GraphicsCommandList7_Vtbl {
        (&raw const self.list7).cast()
    }

    fn unknown_mut(&mut self) -> *mut IUnknown_Vtbl {
        (&raw mut self.list).cast()
    }

    fn table_mut(&mut self) -> *mut ID3D12GraphicsCommandList_Vtbl {
        (&raw mut self.list).cast()
    }

    fn table7_mut(&mut self) -> *mut ID3D12GraphicsCommandList7_Vtbl {
        (&raw mut self.list7).cast()
    }
}

/// Overlays every device interface generation on one allocation, mirroring
/// [`ID3D12GraphicsCommandListVtable`].
#[repr(C)]
pub(crate) union ID3D12DeviceVtable {
    raw: [usize; DEVICE_SLOT_CAPACITY],
    device: ManuallyDrop<ID3D12Device_Vtbl>,
    device1: ManuallyDrop<ID3D12Device1_Vtbl>,
    device2: ManuallyDrop<ID3D12Device2_Vtbl>,
    device3: ManuallyDrop<ID3D12Device3_Vtbl>,
    device4: ManuallyDrop<ID3D12Device4_Vtbl>,
    device5: ManuallyDrop<ID3D12Device5_Vtbl>,
    device6: ManuallyDrop<ID3D12Device6_Vtbl>,
    device7: ManuallyDrop<ID3D12Device7_Vtbl>,
    device8: ManuallyDrop<ID3D12Device8_Vtbl>,
    device9: ManuallyDrop<ID3D12Device9_Vtbl>,
    device10: ManuallyDrop<ID3D12Device10_Vtbl>,
    device11: ManuallyDrop<ID3D12Device11_Vtbl>,
    device12: ManuallyDrop<ID3D12Device12_Vtbl>,
    device13: ManuallyDrop<ID3D12Device13_Vtbl>,
    device14: ManuallyDrop<ID3D12Device14_Vtbl>,
}

const _: () = assert!(size_of::<ID3D12DeviceVtable>() == DEVICE_SLOT_CAPACITY * SLOT_SIZE);

impl ID3D12DeviceVtable {
    /// # Safety
    ///
    /// `original` must point to a vtable spanning at least `slots` entries.
    unsafe fn read(original: NonNull<usize>, slots: usize) -> Self {
        let mut cloned = Self {
            raw: [0; DEVICE_SLOT_CAPACITY],
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
        (&raw const self.device).cast()
    }

    fn table(&self) -> *const ID3D12Device_Vtbl {
        (&raw const self.device).cast()
    }

    fn table4(&self) -> *const ID3D12Device4_Vtbl {
        (&raw const self.device4).cast()
    }

    fn unknown_mut(&mut self) -> *mut IUnknown_Vtbl {
        (&raw mut self.device).cast()
    }

    fn table_mut(&mut self) -> *mut ID3D12Device_Vtbl {
        (&raw mut self.device).cast()
    }

    fn table4_mut(&mut self) -> *mut ID3D12Device4_Vtbl {
        (&raw mut self.device4).cast()
    }
}

pub(crate) struct ID3D12GraphicsCommandListHook<A: Vtable> {
    /// Hook-patched clone installed as the object's vtable pointer.
    patched: Box<ID3D12GraphicsCommandListVtable>,
    /// Verbatim copy of the original slots for downstream continuation.
    saved: ID3D12GraphicsCommandListVtable,
    /// The object's original vtable pointer, restored across releases.
    original: NonNull<usize>,
    /// The widest interface generation cloned from the object's vtable.
    vtable: PhantomData<A>,
}

// SAFETY: `original` identifies an immutable vtable owned by the hooked
// module for the process lifetime; the hook only reads through it.
unsafe impl<A: Vtable> Send for ID3D12GraphicsCommandListHook<A> {}
unsafe impl<A: Vtable> Sync for ID3D12GraphicsCommandListHook<A> {}

impl<A: Vtable> ID3D12GraphicsCommandListHook<A> {
    /// # Safety
    ///
    /// `original` must be null or point to a vtable spanning at least
    /// [`A::SLOTS`](Vtable::SLOTS) entries.
    pub(crate) unsafe fn clone_from(original: *const usize) -> Option<Self> {
        let original = NonNull::new(original.cast_mut())?;
        let saved = unsafe { ID3D12GraphicsCommandListVtable::read(original, A::SLOTS) };
        let mut patched =
            Box::new(unsafe { ID3D12GraphicsCommandListVtable::read(original, A::SLOTS) });
        // SAFETY: the slots written below were copied above, so each place
        // projection targets an initialized entry within the clone.
        unsafe {
            let unknown = patched.unknown_mut();
            (*unknown).Release = crate::tracking::command_list_release;
            let table = patched.table_mut();
            (*table).Reset = crate::tracking::reset;
            (*table).RSSetViewports = crate::tracking::set_viewports;
            (*table).RSSetScissorRects = crate::tracking::set_scissors;
            (*table).DrawInstanced = crate::tracking::draw_instanced;
            (*table).DrawIndexedInstanced = crate::tracking::draw_indexed_instanced;
            (*table).ResourceBarrier = crate::tracking::resource_barrier;
            (*table).SetGraphicsRootDescriptorTable =
                crate::tracking::set_graphics_root_descriptor_table;
            (*table).SetGraphicsRootSignature = crate::tracking::set_graphics_root_signature;
            (*table).OMSetRenderTargets = crate::tracking::om_set_render_targets;
            if A::SLOTS >= <ID3D12GraphicsCommandList7_Vtbl as Vtable>::SLOTS {
                let table7 = patched.table7_mut();
                (*table7).Barrier = crate::tracking::enhanced_barrier;
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

impl<A: Vtable> CommandListHook for ID3D12GraphicsCommandListHook<A> {
    fn slots(&self) -> usize {
        A::SLOTS
    }

    fn vtable_type(&self) -> TypeId {
        TypeId::of::<A>()
    }

    fn patched_vtable(&self) -> *const usize {
        std::ptr::from_ref::<ID3D12GraphicsCommandListVtable>(&self.patched).cast()
    }

    fn original_vtable(&self) -> *const usize {
        self.original.as_ptr()
    }

    fn release(&self) -> ReleaseFn {
        unsafe { (*self.saved.unknown()).Release }
    }

    fn reset(&self) -> ResetFn {
        unsafe { (*self.saved.table()).Reset }
    }

    fn set_viewports(&self) -> SetViewportsFn {
        unsafe { (*self.saved.table()).RSSetViewports }
    }

    fn set_scissors(&self) -> SetScissorsFn {
        unsafe { (*self.saved.table()).RSSetScissorRects }
    }

    fn draw_instanced(&self) -> DrawInstancedFn {
        unsafe { (*self.saved.table()).DrawInstanced }
    }

    fn draw_indexed_instanced(&self) -> DrawIndexedInstancedFn {
        unsafe { (*self.saved.table()).DrawIndexedInstanced }
    }

    fn resource_barrier(&self) -> ResourceBarrierFn {
        unsafe { (*self.saved.table()).ResourceBarrier }
    }

    fn set_graphics_root_descriptor_table(&self) -> SetGraphicsRootDescriptorTableFn {
        unsafe { (*self.saved.table()).SetGraphicsRootDescriptorTable }
    }

    fn set_graphics_root_signature(&self) -> SetGraphicsRootSignatureFn {
        unsafe { (*self.saved.table()).SetGraphicsRootSignature }
    }

    fn om_set_render_targets(&self) -> OmSetRenderTargetsFn {
        unsafe { (*self.saved.table()).OMSetRenderTargets }
    }

    fn barrier(&self) -> Option<EnhancedBarrierFn> {
        (A::SLOTS >= <ID3D12GraphicsCommandList7_Vtbl as Vtable>::SLOTS)
            .then(|| unsafe { (*self.saved.table7()).Barrier })
    }
}

pub(crate) struct ID3D12DeviceHook<A: Vtable> {
    /// Hook-patched clone installed as the object's vtable pointer.
    patched: Box<ID3D12DeviceVtable>,
    /// Verbatim copy of the original slots for downstream continuation.
    saved: ID3D12DeviceVtable,
    /// The object's original vtable pointer, restored across releases.
    original: NonNull<usize>,
    /// The widest interface generation cloned from the object's vtable.
    vtable: PhantomData<A>,
}

// SAFETY: see `ID3D12GraphicsCommandListHook`.
unsafe impl<A: Vtable> Send for ID3D12DeviceHook<A> {}
unsafe impl<A: Vtable> Sync for ID3D12DeviceHook<A> {}

impl<A: Vtable> ID3D12DeviceHook<A> {
    /// # Safety
    ///
    /// `original` must be null or point to a vtable spanning at least
    /// [`A::SLOTS`](Vtable::SLOTS) entries.
    pub(crate) unsafe fn clone_from(original: *const usize) -> Option<Self> {
        let original = NonNull::new(original.cast_mut())?;
        let saved = unsafe { ID3D12DeviceVtable::read(original, A::SLOTS) };
        let mut patched = Box::new(unsafe { ID3D12DeviceVtable::read(original, A::SLOTS) });
        // SAFETY: the slots written below were copied above, so each place
        // projection targets an initialized entry within the clone.
        unsafe {
            let unknown = patched.unknown_mut();
            (*unknown).Release = crate::tracking::device_release;
            let table = patched.table_mut();
            (*table).CreateCommandList = crate::tracking::create_command_list;
            (*table).CreateDescriptorHeap = crate::tracking::create_descriptor_heap;
            (*table).CreateShaderResourceView = crate::tracking::create_shader_resource_view;
            (*table).CreateRenderTargetView = crate::tracking::create_render_target_view;
            (*table).CopyDescriptorsSimple = crate::tracking::copy_descriptors_simple;
            if A::SLOTS >= <ID3D12Device4_Vtbl as Vtable>::SLOTS {
                let table4 = patched.table4_mut();
                (*table4).CreateCommandList1 = crate::tracking::create_command_list1;
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

impl<A: Vtable> DeviceHook for ID3D12DeviceHook<A> {
    fn slots(&self) -> usize {
        A::SLOTS
    }

    fn vtable_type(&self) -> TypeId {
        TypeId::of::<A>()
    }

    fn patched_vtable(&self) -> *const usize {
        std::ptr::from_ref::<ID3D12DeviceVtable>(&self.patched).cast()
    }

    fn original_vtable(&self) -> *const usize {
        self.original.as_ptr()
    }

    fn release(&self) -> ReleaseFn {
        unsafe { (*self.saved.unknown()).Release }
    }

    fn create_command_list(&self) -> CreateCommandListFn {
        unsafe { (*self.saved.table()).CreateCommandList }
    }

    fn create_command_list1(&self) -> Option<CreateCommandList1Fn> {
        (A::SLOTS >= <ID3D12Device4_Vtbl as Vtable>::SLOTS)
            .then(|| unsafe { (*self.saved.table4()).CreateCommandList1 })
    }

    fn create_descriptor_heap(&self) -> CreateDescriptorHeapFn {
        unsafe { (*self.saved.table()).CreateDescriptorHeap }
    }

    fn create_shader_resource_view(&self) -> CreateShaderResourceViewFn {
        unsafe { (*self.saved.table()).CreateShaderResourceView }
    }

    fn create_render_target_view(&self) -> CreateRenderTargetViewFn {
        unsafe { (*self.saved.table()).CreateRenderTargetView }
    }

    fn copy_descriptors_simple(&self) -> CopyDescriptorsSimpleFn {
        unsafe { (*self.saved.table()).CopyDescriptorsSimple }
    }
}
