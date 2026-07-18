//! Typed clones of D3D12 device and graphics command-list vtables with
//! interception applied.
//!
//! Each device returned by `D3D12CreateDevice` — and each graphics command
//! list it creates — receives a per-object heap clone of its vtable with the
//! tracked slots redirected, matching the DXGI and DirectInput hook handling.
//! The clone is copied as opaque words (a vtable may legally carry null or
//! reserved slots, so whole-struct reads would assert validity the source
//! cannot guarantee), while individual slots are read and patched through the
//! `windows` crate's vtable definitions overlaid by a union. The trampolines
//! forward to the original method and then hand the observed state to the
//! producer's tracking module, which owns all capture-region bookkeeping.

use dashmap::{DashMap, mapref::entry::Entry};
use std::any::TypeId;
use std::ffi::c_void;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ptr::NonNull;
use std::sync::LazyLock;
use windows::{
    Win32::{
        Foundation::{E_FAIL, RECT},
        Graphics::Direct3D12::{
            D3D12_BARRIER_GROUP, D3D12_COMMAND_LIST_FLAGS, D3D12_COMMAND_LIST_TYPE,
            D3D12_CPU_DESCRIPTOR_HANDLE, D3D12_DESCRIPTOR_HEAP_DESC, D3D12_DESCRIPTOR_HEAP_TYPE,
            D3D12_GPU_DESCRIPTOR_HANDLE, D3D12_RENDER_TARGET_VIEW_DESC, D3D12_RESOURCE_BARRIER,
            D3D12_SHADER_RESOURCE_VIEW_DESC, D3D12_VIEWPORT, ID3D12Device, ID3D12Device_Vtbl,
            ID3D12Device1, ID3D12Device1_Vtbl, ID3D12Device2, ID3D12Device2_Vtbl, ID3D12Device3,
            ID3D12Device3_Vtbl, ID3D12Device4, ID3D12Device4_Vtbl, ID3D12Device5,
            ID3D12Device5_Vtbl, ID3D12Device6, ID3D12Device6_Vtbl, ID3D12Device7,
            ID3D12Device7_Vtbl, ID3D12Device8, ID3D12Device8_Vtbl, ID3D12Device9,
            ID3D12Device9_Vtbl, ID3D12Device10, ID3D12Device10_Vtbl, ID3D12Device11,
            ID3D12Device11_Vtbl, ID3D12Device12, ID3D12Device12_Vtbl, ID3D12Device13,
            ID3D12Device13_Vtbl, ID3D12Device14, ID3D12Device14_Vtbl, ID3D12GraphicsCommandList,
            ID3D12GraphicsCommandList_Vtbl, ID3D12GraphicsCommandList1,
            ID3D12GraphicsCommandList1_Vtbl, ID3D12GraphicsCommandList2,
            ID3D12GraphicsCommandList2_Vtbl, ID3D12GraphicsCommandList3,
            ID3D12GraphicsCommandList3_Vtbl, ID3D12GraphicsCommandList4,
            ID3D12GraphicsCommandList4_Vtbl, ID3D12GraphicsCommandList5,
            ID3D12GraphicsCommandList5_Vtbl, ID3D12GraphicsCommandList6,
            ID3D12GraphicsCommandList6_Vtbl, ID3D12GraphicsCommandList7,
            ID3D12GraphicsCommandList7_Vtbl, ID3D12GraphicsCommandList8,
            ID3D12GraphicsCommandList8_Vtbl, ID3D12GraphicsCommandList9,
            ID3D12GraphicsCommandList9_Vtbl, ID3D12GraphicsCommandList10,
            ID3D12GraphicsCommandList10_Vtbl,
        },
    },
    core::{BOOL, GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface},
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
            (*unknown).Release = command_list_release;
            let table = patched.table_mut();
            (*table).Reset = reset;
            (*table).RSSetViewports = set_viewports;
            (*table).RSSetScissorRects = set_scissors;
            (*table).DrawInstanced = draw_instanced;
            (*table).DrawIndexedInstanced = draw_indexed_instanced;
            (*table).ResourceBarrier = resource_barrier;
            (*table).SetGraphicsRootDescriptorTable = set_graphics_root_descriptor_table;
            (*table).SetGraphicsRootSignature = set_graphics_root_signature;
            (*table).OMSetRenderTargets = om_set_render_targets;
            if A::SLOTS >= <ID3D12GraphicsCommandList7_Vtbl as Vtable>::SLOTS {
                let table7 = patched.table7_mut();
                (*table7).Barrier = enhanced_barrier;
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
            (*unknown).Release = device_release;
            let table = patched.table_mut();
            (*table).CreateCommandList = create_command_list;
            (*table).CreateDescriptorHeap = create_descriptor_heap;
            (*table).CreateShaderResourceView = create_shader_resource_view;
            (*table).CreateRenderTargetView = create_render_target_view;
            (*table).CopyDescriptorsSimple = copy_descriptors_simple;
            if A::SLOTS >= <ID3D12Device4_Vtbl as Vtable>::SLOTS {
                let table4 = patched.table4_mut();
                (*table4).CreateCommandList1 = create_command_list1;
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

static DEVICES: LazyLock<DashMap<usize, Box<dyn DeviceHook>>> = LazyLock::new(Default::default);
static COMMAND_LISTS: LazyLock<DashMap<usize, Box<dyn CommandListHook>>> =
    LazyLock::new(Default::default);

/// A monomorphized per-object install routine, used to dispatch a widest-cast
/// result to a typed layout.
type InstallFn = unsafe fn(*mut c_void) -> bool;

/// Installs render-target tracking on a device returned by
/// `D3D12CreateDevice`. Command lists are hooked as the device creates them.
///
/// # Safety
///
/// `output` must be null or point to a COM interface pointer for a live
/// `ID3D12Device`.
pub(crate) unsafe fn install_device(output: *mut *mut c_void) {
    if std::env::var_os("SNOWGLASS_TRACK_RENDER_TARGET").is_none() || output.is_null() {
        return;
    }
    match unsafe { try_install_device(output) } {
        Ok(true) => crate::d3d12_log("snowglass: installed D3D12 viewport/scissor tracking"),
        Ok(false) => {}
        Err(error) => crate::d3d12_log(format!(
            "snowglass: failed to install D3D12 viewport/scissor tracking: {error}"
        )),
    }
}

unsafe fn try_install_device(output: *mut *mut c_void) -> Result<bool, String> {
    let raw = unsafe { *output };
    let unknown = unsafe { IUnknown::from_raw_borrowed(&raw) }
        .ok_or_else(|| "D3D12CreateDevice returned a null device".to_owned())?;
    let device: ID3D12Device = unknown.cast().map_err(|error| error.to_string())?;
    if let Some(enhanced_barriers) = snowglass_producer_d3d12::detect_device_features(&device)? {
        crate::d3d12_log(if enhanced_barriers {
            "snowglass: D3D12 enhanced capture barriers enabled"
        } else {
            "snowglass: D3D12 capture using legacy resource barriers"
        });
    }
    let base = device.as_raw();
    let already_hooked = DEVICES
        .get(&(base as usize))
        .is_some_and(|hook| unsafe { *base.cast::<*const usize>() } == hook.patched_vtable());
    // Resolve every interface pointer before replacing any vtable. D3D12
    // wrappers may expose distinct pointers for intermediate generations, so
    // patching only the base and widest interfaces misses calls made through
    // (for example) ID3D12Device4. The list is widest-first and deduplicated,
    // ensuring each physical pointer is cloned exactly once.
    let interfaces = device_interfaces(&device);
    let mut installed = false;
    for (interface, install) in interfaces {
        installed |= unsafe { install(interface) };
    }
    if !installed {
        return Err("could not clone the device vtable".to_owned());
    }
    Ok(!already_hooked)
}

fn device_interfaces(device: &ID3D12Device) -> Vec<(*mut c_void, InstallFn)> {
    let mut interfaces = Vec::new();
    macro_rules! try_cast {
        ($interface:ty, $vtbl:ty) => {
            if let Ok(interface) = device.cast::<$interface>() {
                let pointer = interface.as_raw();
                if !interfaces.iter().any(|(existing, _)| *existing == pointer) {
                    interfaces.push((pointer, install_device_kind::<$vtbl> as InstallFn));
                }
            }
        };
    }
    try_cast!(ID3D12Device14, ID3D12Device14_Vtbl);
    try_cast!(ID3D12Device13, ID3D12Device13_Vtbl);
    try_cast!(ID3D12Device12, ID3D12Device12_Vtbl);
    try_cast!(ID3D12Device11, ID3D12Device11_Vtbl);
    try_cast!(ID3D12Device10, ID3D12Device10_Vtbl);
    try_cast!(ID3D12Device9, ID3D12Device9_Vtbl);
    try_cast!(ID3D12Device8, ID3D12Device8_Vtbl);
    try_cast!(ID3D12Device7, ID3D12Device7_Vtbl);
    try_cast!(ID3D12Device6, ID3D12Device6_Vtbl);
    try_cast!(ID3D12Device5, ID3D12Device5_Vtbl);
    try_cast!(ID3D12Device4, ID3D12Device4_Vtbl);
    try_cast!(ID3D12Device3, ID3D12Device3_Vtbl);
    try_cast!(ID3D12Device2, ID3D12Device2_Vtbl);
    try_cast!(ID3D12Device1, ID3D12Device1_Vtbl);
    let base = device.as_raw();
    if !interfaces.iter().any(|(existing, _)| *existing == base) {
        interfaces.push((base, install_device_kind::<ID3D12Device_Vtbl> as InstallFn));
    }
    interfaces
}

unsafe fn install_device_kind<A: Vtable>(device: *mut c_void) -> bool {
    if device.is_null() {
        return false;
    }
    let entry = DEVICES.entry(device as usize);
    let current = unsafe { *device.cast::<*const usize>() };
    let vtable = match &entry {
        Entry::Occupied(occupied) => {
            let existing = occupied.get();
            if current == existing.patched_vtable() {
                if existing.vtable_type() == TypeId::of::<A>() || existing.slots() >= A::SLOTS {
                    return true;
                }
                unsafe { ID3D12DeviceHook::<A>::clone_from(existing.original_vtable()) }
            } else {
                unsafe { ID3D12DeviceHook::<A>::clone_from(current) }
            }
        }
        Entry::Vacant(_) => unsafe { ID3D12DeviceHook::<A>::clone_from(current) },
    };
    let Some(vtable) = vtable else {
        return false;
    };
    let state: Box<dyn DeviceHook> = Box::new(vtable);
    unsafe { *device.cast::<*const usize>() = state.patched_vtable() };
    entry.insert(state);
    true
}

/// Installs tracking interception on a command list created by a hooked
/// device.
///
/// # Safety
///
/// `list` must be null or point to a live COM command-list interface.
unsafe fn install_command_list(list: *mut c_void, interface_id: *const GUID) {
    if list.is_null() || interface_id.is_null() {
        return;
    }
    let Some(unknown) = (unsafe { IUnknown::from_raw_borrowed(&list) }) else {
        return;
    };
    let Ok(base) = unknown.cast::<ID3D12GraphicsCommandList>() else {
        return;
    };
    // Clone the widest interface generation the object implements rather than
    // the generation it was created as: a caller can obtain a wider interface
    // later through QueryInterface (typically the same pointer), and a clone
    // sized to the creation interface would dispatch those wider slots into
    // the uninitialized tail of the clone.
    let mut interfaces = Vec::new();
    macro_rules! try_cast {
        ($interface:ty, $vtbl:ty) => {
            if let Ok(interface) = base.cast::<$interface>() {
                let pointer = interface.as_raw();
                if !interfaces.iter().any(|(existing, _)| *existing == pointer) {
                    interfaces.push((pointer, install_command_list_kind::<$vtbl> as InstallFn));
                }
            }
        };
    }
    try_cast!(
        ID3D12GraphicsCommandList10,
        ID3D12GraphicsCommandList10_Vtbl
    );
    try_cast!(ID3D12GraphicsCommandList9, ID3D12GraphicsCommandList9_Vtbl);
    try_cast!(ID3D12GraphicsCommandList8, ID3D12GraphicsCommandList8_Vtbl);
    try_cast!(ID3D12GraphicsCommandList7, ID3D12GraphicsCommandList7_Vtbl);
    try_cast!(ID3D12GraphicsCommandList6, ID3D12GraphicsCommandList6_Vtbl);
    try_cast!(ID3D12GraphicsCommandList5, ID3D12GraphicsCommandList5_Vtbl);
    try_cast!(ID3D12GraphicsCommandList4, ID3D12GraphicsCommandList4_Vtbl);
    try_cast!(ID3D12GraphicsCommandList3, ID3D12GraphicsCommandList3_Vtbl);
    try_cast!(ID3D12GraphicsCommandList2, ID3D12GraphicsCommandList2_Vtbl);
    try_cast!(ID3D12GraphicsCommandList1, ID3D12GraphicsCommandList1_Vtbl);
    let base_pointer = base.as_raw();
    if !interfaces
        .iter()
        .any(|(existing, _)| *existing == base_pointer)
    {
        interfaces.push((
            base_pointer,
            install_command_list_kind::<ID3D12GraphicsCommandList_Vtbl> as InstallFn,
        ));
    }
    for (pointer, install) in interfaces {
        unsafe { install(pointer) };
    }
}

unsafe fn install_command_list_kind<A: Vtable>(list: *mut c_void) -> bool {
    if list.is_null() {
        return false;
    }
    let entry = COMMAND_LISTS.entry(list as usize);
    let current = unsafe { *list.cast::<*const usize>() };
    let vtable = match &entry {
        Entry::Occupied(occupied) => {
            let existing = occupied.get();
            if current == existing.patched_vtable() {
                if existing.vtable_type() == TypeId::of::<A>() || existing.slots() >= A::SLOTS {
                    return true;
                }
                unsafe {
                    ID3D12GraphicsCommandListHook::<A>::clone_from(existing.original_vtable())
                }
            } else {
                unsafe { ID3D12GraphicsCommandListHook::<A>::clone_from(current) }
            }
        }
        Entry::Vacant(_) => unsafe { ID3D12GraphicsCommandListHook::<A>::clone_from(current) },
    };
    let Some(vtable) = vtable else {
        return false;
    };
    let state: Box<dyn CommandListHook> = Box::new(vtable);
    unsafe { *list.cast::<*const usize>() = state.patched_vtable() };
    entry.insert(state);
    true
}

fn device_method<T>(device: *mut c_void, select: impl FnOnce(&dyn DeviceHook) -> T) -> Option<T> {
    DEVICES.get(&(device as usize)).map(|hook| select(&**hook))
}

fn list_method<T>(list: *mut c_void, select: impl FnOnce(&dyn CommandListHook) -> T) -> Option<T> {
    COMMAND_LISTS
        .get(&(list as usize))
        .map(|hook| select(&**hook))
}

unsafe extern "system" fn device_release(this: *mut c_void) -> u32 {
    let Some((original, release)) =
        device_method(this, |hook| (hook.original_vtable(), hook.release()))
    else {
        return 0;
    };
    // The cloned vtable spans only the COM interface methods, but a final
    // release destroys the object through implementation virtuals stored
    // beyond them. Restore the original vtable across the downstream release
    // so destruction never reads past the clone.
    unsafe { *this.cast::<*const usize>() = original };
    let references = unsafe { release(this) };
    if references == 0 {
        DEVICES.remove(&(this as usize));
    } else if let Some(patched) = device_method(this, |hook| hook.patched_vtable()) {
        unsafe { *this.cast::<*const usize>() = patched };
    }
    references
}

unsafe extern "system" fn command_list_release(this: *mut c_void) -> u32 {
    let Some((original, release)) =
        list_method(this, |hook| (hook.original_vtable(), hook.release()))
    else {
        return 0;
    };
    // See `device_release`: destruction must run on the original, full-length
    // vtable rather than the interface-only clone.
    unsafe { *this.cast::<*const usize>() = original };
    let references = unsafe { release(this) };
    if references == 0 {
        COMMAND_LISTS.remove(&(this as usize));
    } else if let Some(patched) = list_method(this, |hook| hook.patched_vtable()) {
        unsafe { *this.cast::<*const usize>() = patched };
    }
    references
}

unsafe extern "system" fn create_command_list(
    this: *mut c_void,
    node_mask: u32,
    list_type: D3D12_COMMAND_LIST_TYPE,
    allocator: *mut c_void,
    initial_state: *mut c_void,
    interface_id: *const GUID,
    output: *mut *mut c_void,
) -> HRESULT {
    let Some(original) = device_method(this, |hook| hook.create_command_list()) else {
        return E_FAIL;
    };
    let result = unsafe {
        original(
            this,
            node_mask,
            list_type,
            allocator,
            initial_state,
            interface_id,
            output,
        )
    };
    if result.is_ok() && !output.is_null() {
        unsafe { install_command_list(*output, interface_id) };
    }
    result
}

unsafe extern "system" fn create_command_list1(
    this: *mut c_void,
    node_mask: u32,
    list_type: D3D12_COMMAND_LIST_TYPE,
    flags: D3D12_COMMAND_LIST_FLAGS,
    interface_id: *const GUID,
    output: *mut *mut c_void,
) -> HRESULT {
    let Some(original) = device_method(this, |hook| hook.create_command_list1()).flatten() else {
        return E_FAIL;
    };
    let result = unsafe { original(this, node_mask, list_type, flags, interface_id, output) };
    if result.is_ok() && !output.is_null() {
        unsafe { install_command_list(*output, interface_id) };
    }
    result
}

unsafe extern "system" fn create_descriptor_heap(
    this: *mut c_void,
    desc: *const D3D12_DESCRIPTOR_HEAP_DESC,
    interface_id: *const GUID,
    output: *mut *mut c_void,
) -> HRESULT {
    let Some(original) = device_method(this, |hook| hook.create_descriptor_heap()) else {
        return E_FAIL;
    };
    let result = unsafe { original(this, desc, interface_id, output) };
    if result.is_ok() {
        unsafe { snowglass_producer_d3d12::record_descriptor_heap(desc, output) };
    }
    result
}

unsafe extern "system" fn create_shader_resource_view(
    this: *mut c_void,
    resource: *mut c_void,
    desc: *const D3D12_SHADER_RESOURCE_VIEW_DESC,
    destination: D3D12_CPU_DESCRIPTOR_HANDLE,
) {
    if let Some(original) = device_method(this, |hook| hook.create_shader_resource_view()) {
        unsafe { original(this, resource, desc, destination) };
    }
    unsafe { snowglass_producer_d3d12::record_shader_resource_view(resource, desc, destination) };
}

unsafe extern "system" fn create_render_target_view(
    this: *mut c_void,
    resource: *mut c_void,
    desc: *const D3D12_RENDER_TARGET_VIEW_DESC,
    destination: D3D12_CPU_DESCRIPTOR_HANDLE,
) {
    if let Some(original) = device_method(this, |hook| hook.create_render_target_view()) {
        unsafe { original(this, resource, desc, destination) };
    }
    unsafe { snowglass_producer_d3d12::record_render_target_view(resource, desc, destination) };
}

unsafe extern "system" fn copy_descriptors_simple(
    this: *mut c_void,
    count: u32,
    destination: D3D12_CPU_DESCRIPTOR_HANDLE,
    source: D3D12_CPU_DESCRIPTOR_HANDLE,
    heap_type: D3D12_DESCRIPTOR_HEAP_TYPE,
) {
    if let Some(original) = device_method(this, |hook| hook.copy_descriptors_simple()) {
        unsafe { original(this, count, destination, source, heap_type) };
    }
    snowglass_producer_d3d12::record_copy_descriptors_simple(count, destination, source, heap_type);
}

unsafe extern "system" fn reset(
    this: *mut c_void,
    allocator: *mut c_void,
    initial_state: *mut c_void,
) -> HRESULT {
    let Some(original) = list_method(this, |hook| hook.reset()) else {
        return E_FAIL;
    };
    let result = unsafe { original(this, allocator, initial_state) };
    // d3d12.dll re-initializes the recycled command list inside Reset and
    // rewrites the object's vtable pointer, discarding the per-object clone.
    // Reinstall the clone after forwarding so tracking survives the reset.
    if let Some(patched) = list_method(this, |hook| hook.patched_vtable()) {
        unsafe { *this.cast::<*const usize>() = patched };
    }
    if result.is_ok() {
        snowglass_producer_d3d12::command_reset(this);
    }
    result
}

unsafe extern "system" fn set_viewports(
    this: *mut c_void,
    count: u32,
    viewports: *const D3D12_VIEWPORT,
) {
    if let Some(original) = list_method(this, |hook| hook.set_viewports()) {
        unsafe { original(this, count, viewports) };
    }
    unsafe { snowglass_producer_d3d12::record_viewports(this, count, viewports) };
}

unsafe extern "system" fn set_scissors(this: *mut c_void, count: u32, scissors: *const RECT) {
    if let Some(original) = list_method(this, |hook| hook.set_scissors()) {
        unsafe { original(this, count, scissors) };
    }
    unsafe { snowglass_producer_d3d12::record_scissors(this, count, scissors) };
}

unsafe extern "system" fn draw_instanced(
    this: *mut c_void,
    vertex_count: u32,
    instance_count: u32,
    start_vertex: u32,
    start_instance: u32,
) {
    if let Some(original) = list_method(this, |hook| hook.draw_instanced()) {
        unsafe {
            original(
                this,
                vertex_count,
                instance_count,
                start_vertex,
                start_instance,
            )
        };
    }
    snowglass_producer_d3d12::record_draw(this);
}

unsafe extern "system" fn draw_indexed_instanced(
    this: *mut c_void,
    index_count: u32,
    instance_count: u32,
    start_index: u32,
    base_vertex: i32,
    start_instance: u32,
) {
    if let Some(original) = list_method(this, |hook| hook.draw_indexed_instanced()) {
        unsafe {
            original(
                this,
                index_count,
                instance_count,
                start_index,
                base_vertex,
                start_instance,
            )
        };
    }
    snowglass_producer_d3d12::record_draw(this);
}

unsafe extern "system" fn resource_barrier(
    this: *mut c_void,
    count: u32,
    barriers: *const D3D12_RESOURCE_BARRIER,
) {
    if let Some(original) = list_method(this, |hook| hook.resource_barrier()) {
        unsafe { original(this, count, barriers) };
    }
    unsafe { snowglass_producer_d3d12::record_resource_barrier(this, count, barriers) };
}

unsafe extern "system" fn set_graphics_root_descriptor_table(
    this: *mut c_void,
    root_index: u32,
    descriptor: D3D12_GPU_DESCRIPTOR_HANDLE,
) {
    if let Some(original) = list_method(this, |hook| hook.set_graphics_root_descriptor_table()) {
        unsafe { original(this, root_index, descriptor) };
    }
    snowglass_producer_d3d12::record_root_descriptor_table(this, root_index, descriptor);
}

unsafe extern "system" fn set_graphics_root_signature(this: *mut c_void, signature: *mut c_void) {
    if let Some(original) = list_method(this, |hook| hook.set_graphics_root_signature()) {
        unsafe { original(this, signature) };
    }
    snowglass_producer_d3d12::record_root_signature(this);
}

unsafe extern "system" fn om_set_render_targets(
    this: *mut c_void,
    count: u32,
    descriptors: *const D3D12_CPU_DESCRIPTOR_HANDLE,
    contiguous: BOOL,
    depth: *const D3D12_CPU_DESCRIPTOR_HANDLE,
) {
    if let Some(original) = list_method(this, |hook| hook.om_set_render_targets()) {
        unsafe { original(this, count, descriptors, contiguous, depth) };
    }
    unsafe { snowglass_producer_d3d12::record_render_targets(this, count, descriptors) };
}

unsafe extern "system" fn enhanced_barrier(
    this: *mut c_void,
    count: u32,
    groups: *const D3D12_BARRIER_GROUP,
) {
    if let Some(original) = list_method(this, |hook| hook.barrier()).flatten() {
        unsafe { original(this, count, groups) };
    }
    unsafe { snowglass_producer_d3d12::record_enhanced_barrier(this, count, groups) };
}
