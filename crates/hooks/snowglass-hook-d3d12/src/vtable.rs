//! Typed clones of D3D12 device and graphics command-list vtables with
//! interception applied.
//!
//! Each hooked object receives a heap clone of its vtable with the tracked
//! slots redirected. The clone is copied as opaque words (a vtable may legally
//! carry null or reserved slots, so whole-struct reads would assert validity
//! the source cannot guarantee), while individual slots are read and patched
//! through the `windows` crate's vtable definitions overlaid by a union.

use std::ffi::c_void;
use std::mem::ManuallyDrop;
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

const SLOT_SIZE: usize = size_of::<usize>();
const LIST_SLOT_CAPACITY: usize = size_of::<ID3D12GraphicsCommandList10_Vtbl>() / SLOT_SIZE;
const DEVICE_SLOT_CAPACITY: usize = size_of::<ID3D12Device14_Vtbl>() / SLOT_SIZE;

/// The widest graphics command-list interface generation cloned from an
/// object's vtable.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum GraphicsCommandListKind {
    List,
    List1,
    List2,
    List3,
    List4,
    List5,
    List6,
    List7,
    List8,
    List9,
    List10,
}

impl GraphicsCommandListKind {
    pub(crate) const fn slots(self) -> usize {
        (match self {
            Self::List => size_of::<ID3D12GraphicsCommandList_Vtbl>(),
            Self::List1 => size_of::<ID3D12GraphicsCommandList1_Vtbl>(),
            Self::List2 => size_of::<ID3D12GraphicsCommandList2_Vtbl>(),
            Self::List3 => size_of::<ID3D12GraphicsCommandList3_Vtbl>(),
            Self::List4 => size_of::<ID3D12GraphicsCommandList4_Vtbl>(),
            Self::List5 => size_of::<ID3D12GraphicsCommandList5_Vtbl>(),
            Self::List6 => size_of::<ID3D12GraphicsCommandList6_Vtbl>(),
            Self::List7 => size_of::<ID3D12GraphicsCommandList7_Vtbl>(),
            Self::List8 => size_of::<ID3D12GraphicsCommandList8_Vtbl>(),
            Self::List9 => size_of::<ID3D12GraphicsCommandList9_Vtbl>(),
            Self::List10 => size_of::<ID3D12GraphicsCommandList10_Vtbl>(),
        }) / SLOT_SIZE
    }
}

/// The widest device interface generation cloned from an object's vtable.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum DeviceKind {
    Device,
    Device1,
    Device2,
    Device3,
    Device4,
    Device5,
    Device6,
    Device7,
    Device8,
    Device9,
    Device10,
    Device11,
    Device12,
    Device13,
    Device14,
}

impl DeviceKind {
    pub(crate) const fn slots(self) -> usize {
        (match self {
            Self::Device => size_of::<ID3D12Device_Vtbl>(),
            Self::Device1 => size_of::<ID3D12Device1_Vtbl>(),
            Self::Device2 => size_of::<ID3D12Device2_Vtbl>(),
            Self::Device3 => size_of::<ID3D12Device3_Vtbl>(),
            Self::Device4 => size_of::<ID3D12Device4_Vtbl>(),
            Self::Device5 => size_of::<ID3D12Device5_Vtbl>(),
            Self::Device6 => size_of::<ID3D12Device6_Vtbl>(),
            Self::Device7 => size_of::<ID3D12Device7_Vtbl>(),
            Self::Device8 => size_of::<ID3D12Device8_Vtbl>(),
            Self::Device9 => size_of::<ID3D12Device9_Vtbl>(),
            Self::Device10 => size_of::<ID3D12Device10_Vtbl>(),
            Self::Device11 => size_of::<ID3D12Device11_Vtbl>(),
            Self::Device12 => size_of::<ID3D12Device12_Vtbl>(),
            Self::Device13 => size_of::<ID3D12Device13_Vtbl>(),
            Self::Device14 => size_of::<ID3D12Device14_Vtbl>(),
        }) / SLOT_SIZE
    }
}

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
    /// `original` must point to a vtable spanning at least `kind.slots()`
    /// entries.
    unsafe fn read(original: *const usize, kind: GraphicsCommandListKind) -> Self {
        let mut slots = Self {
            raw: [0; LIST_SLOT_CAPACITY],
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
    /// `original` must point to a vtable spanning at least `kind.slots()`
    /// entries.
    unsafe fn read(original: *const usize, kind: DeviceKind) -> Self {
        let mut slots = Self {
            raw: [0; DEVICE_SLOT_CAPACITY],
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

pub(crate) struct ID3D12GraphicsCommandListHook {
    /// Hook-patched clone installed as the object's vtable pointer.
    patched: Box<ID3D12GraphicsCommandListVtable>,
    /// Verbatim copy of the original slots for downstream continuation.
    saved: ID3D12GraphicsCommandListVtable,
    /// The object's original vtable pointer, restored across releases.
    original: usize,
    kind: GraphicsCommandListKind,
}

impl ID3D12GraphicsCommandListHook {
    /// # Safety
    ///
    /// `original` must be null or point to a vtable spanning at least
    /// `kind.slots()` entries.
    pub(crate) unsafe fn clone_from(
        original: *const usize,
        kind: GraphicsCommandListKind,
    ) -> Option<Self> {
        if original.is_null() {
            return None;
        }
        let saved = unsafe { ID3D12GraphicsCommandListVtable::read(original, kind) };
        let mut patched = Box::new(unsafe { ID3D12GraphicsCommandListVtable::read(original, kind) });
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
            if kind >= GraphicsCommandListKind::List7 {
                let table7 = patched.table7_mut();
                (*table7).Barrier = crate::tracking::enhanced_barrier;
            }
        }
        Some(Self {
            patched,
            saved,
            original: original as usize,
            kind,
        })
    }

    pub(crate) fn kind(&self) -> GraphicsCommandListKind {
        self.kind
    }

    /// The vtable pointer that intercepts a hooked object.
    pub(crate) fn patched_vtable(&self) -> *const usize {
        std::ptr::from_ref::<ID3D12GraphicsCommandListVtable>(&self.patched).cast()
    }

    /// The object's original vtable pointer.
    pub(crate) fn original_vtable(&self) -> *const usize {
        self.original as *const usize
    }

    pub(crate) fn release(&self) -> ReleaseFn {
        unsafe { (*self.saved.unknown()).Release }
    }

    pub(crate) fn reset(&self) -> ResetFn {
        unsafe { (*self.saved.table()).Reset }
    }

    pub(crate) fn set_viewports(&self) -> SetViewportsFn {
        unsafe { (*self.saved.table()).RSSetViewports }
    }

    pub(crate) fn set_scissors(&self) -> SetScissorsFn {
        unsafe { (*self.saved.table()).RSSetScissorRects }
    }

    pub(crate) fn draw_instanced(&self) -> DrawInstancedFn {
        unsafe { (*self.saved.table()).DrawInstanced }
    }

    pub(crate) fn draw_indexed_instanced(&self) -> DrawIndexedInstancedFn {
        unsafe { (*self.saved.table()).DrawIndexedInstanced }
    }

    pub(crate) fn resource_barrier(&self) -> ResourceBarrierFn {
        unsafe { (*self.saved.table()).ResourceBarrier }
    }

    pub(crate) fn set_graphics_root_descriptor_table(&self) -> SetGraphicsRootDescriptorTableFn {
        unsafe { (*self.saved.table()).SetGraphicsRootDescriptorTable }
    }

    pub(crate) fn set_graphics_root_signature(&self) -> SetGraphicsRootSignatureFn {
        unsafe { (*self.saved.table()).SetGraphicsRootSignature }
    }

    pub(crate) fn om_set_render_targets(&self) -> OmSetRenderTargetsFn {
        unsafe { (*self.saved.table()).OMSetRenderTargets }
    }

    pub(crate) fn barrier(&self) -> Option<EnhancedBarrierFn> {
        (self.kind >= GraphicsCommandListKind::List7)
            .then(|| unsafe { (*self.saved.table7()).Barrier })
    }
}

pub(crate) struct ID3D12DeviceHook {
    /// Hook-patched clone installed as the object's vtable pointer.
    patched: Box<ID3D12DeviceVtable>,
    /// Verbatim copy of the original slots for downstream continuation.
    saved: ID3D12DeviceVtable,
    /// The object's original vtable pointer, restored across releases.
    original: usize,
    kind: DeviceKind,
}

impl ID3D12DeviceHook {
    /// # Safety
    ///
    /// `original` must be null or point to a vtable spanning at least
    /// `kind.slots()` entries.
    pub(crate) unsafe fn clone_from(original: *const usize, kind: DeviceKind) -> Option<Self> {
        if original.is_null() {
            return None;
        }
        let saved = unsafe { ID3D12DeviceVtable::read(original, kind) };
        let mut patched = Box::new(unsafe { ID3D12DeviceVtable::read(original, kind) });
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
            if kind >= DeviceKind::Device4 {
                let table4 = patched.table4_mut();
                (*table4).CreateCommandList1 = crate::tracking::create_command_list1;
            }
        }
        Some(Self {
            patched,
            saved,
            original: original as usize,
            kind,
        })
    }

    pub(crate) fn kind(&self) -> DeviceKind {
        self.kind
    }

    /// The vtable pointer that intercepts a hooked object.
    pub(crate) fn patched_vtable(&self) -> *const usize {
        std::ptr::from_ref::<ID3D12DeviceVtable>(&self.patched).cast()
    }

    /// The object's original vtable pointer.
    pub(crate) fn original_vtable(&self) -> *const usize {
        self.original as *const usize
    }

    pub(crate) fn release(&self) -> ReleaseFn {
        unsafe { (*self.saved.unknown()).Release }
    }

    pub(crate) fn create_command_list(&self) -> CreateCommandListFn {
        unsafe { (*self.saved.table()).CreateCommandList }
    }

    pub(crate) fn create_command_list1(&self) -> Option<CreateCommandList1Fn> {
        (self.kind >= DeviceKind::Device4)
            .then(|| unsafe { (*self.saved.table4()).CreateCommandList1 })
    }

    pub(crate) fn create_descriptor_heap(&self) -> CreateDescriptorHeapFn {
        unsafe { (*self.saved.table()).CreateDescriptorHeap }
    }

    pub(crate) fn create_shader_resource_view(&self) -> CreateShaderResourceViewFn {
        unsafe { (*self.saved.table()).CreateShaderResourceView }
    }

    pub(crate) fn create_render_target_view(&self) -> CreateRenderTargetViewFn {
        unsafe { (*self.saved.table()).CreateRenderTargetView }
    }

    pub(crate) fn copy_descriptors_simple(&self) -> CopyDescriptorsSimpleFn {
        unsafe { (*self.saved.table()).CopyDescriptorsSimple }
    }
}
