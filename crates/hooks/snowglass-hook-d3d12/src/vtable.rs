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
use std::ptr::NonNull;
use std::sync::{Arc, LazyLock};
use windows::{
    Win32::{
        Foundation::{E_FAIL, RECT},
        Graphics::Direct3D12::{
            D3D12_BARRIER_GROUP, D3D12_COMMAND_LIST_FLAGS, D3D12_COMMAND_LIST_TYPE,
            D3D12_CPU_DESCRIPTOR_HANDLE, D3D12_DESCRIPTOR_HEAP_DESC, D3D12_DESCRIPTOR_HEAP_TYPE,
            D3D12_GPU_DESCRIPTOR_HANDLE, D3D12_RENDER_TARGET_VIEW_DESC, D3D12_RESOURCE_BARRIER,
            D3D12_SHADER_RESOURCE_VIEW_DESC, D3D12_VIEWPORT, ID3D12CommandQueue, ID3D12Device,
            ID3D12Device_Vtbl, ID3D12Device1, ID3D12Device1_Vtbl, ID3D12Device2,
            ID3D12Device2_Vtbl, ID3D12Device3, ID3D12Device3_Vtbl, ID3D12Device4,
            ID3D12Device4_Vtbl, ID3D12Device5, ID3D12Device5_Vtbl, ID3D12Device6,
            ID3D12Device6_Vtbl, ID3D12Device7, ID3D12Device7_Vtbl, ID3D12Device8,
            ID3D12Device8_Vtbl, ID3D12Device9, ID3D12Device9_Vtbl, ID3D12Device10,
            ID3D12Device10_Vtbl, ID3D12Device11, ID3D12Device11_Vtbl, ID3D12Device12,
            ID3D12Device12_Vtbl, ID3D12Device13, ID3D12Device13_Vtbl, ID3D12Device14,
            ID3D12Device14_Vtbl, ID3D12GraphicsCommandList, ID3D12GraphicsCommandList_Vtbl,
            ID3D12GraphicsCommandList1, ID3D12GraphicsCommandList1_Vtbl,
            ID3D12GraphicsCommandList2, ID3D12GraphicsCommandList2_Vtbl,
            ID3D12GraphicsCommandList3, ID3D12GraphicsCommandList3_Vtbl,
            ID3D12GraphicsCommandList4, ID3D12GraphicsCommandList4_Vtbl,
            ID3D12GraphicsCommandList5, ID3D12GraphicsCommandList5_Vtbl,
            ID3D12GraphicsCommandList6, ID3D12GraphicsCommandList6_Vtbl,
            ID3D12GraphicsCommandList7, ID3D12GraphicsCommandList7_Vtbl,
            ID3D12GraphicsCommandList8, ID3D12GraphicsCommandList8_Vtbl,
            ID3D12GraphicsCommandList9, ID3D12GraphicsCommandList9_Vtbl,
            ID3D12GraphicsCommandList10, ID3D12GraphicsCommandList10_Vtbl,
        },
    },
    core::{BOOL, GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface},
};

snowglass_hook::vtable! {
    [
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
    ],
    [
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
    ],
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

/// The layout-erased view of a hooked graphics command list stored in the
/// tracking registry.
pub(crate) trait CommandListHook: snowglass_hook::VtableHook {
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
pub(crate) trait DeviceHook: snowglass_hook::VtableHook {
    fn release(&self) -> ReleaseFn;
    fn create_command_list(&self) -> CreateCommandListFn;
    fn create_command_list1(&self) -> Option<CreateCommandList1Fn>;
    fn create_descriptor_heap(&self) -> CreateDescriptorHeapFn;
    fn create_shader_resource_view(&self) -> CreateShaderResourceViewFn;
    fn create_render_target_view(&self) -> CreateRenderTargetViewFn;
    fn copy_descriptors_simple(&self) -> CopyDescriptorsSimpleFn;
}

snowglass_hook::com_vtable! {
    /// Overlays every graphics command-list interface generation on one
    /// allocation. Narrower interfaces are prefixes of this storage.
    pub(crate) union ID3D12GraphicsCommandListVtable for ID3D12GraphicsCommandList10_Vtbl;
}

snowglass_hook::com_vtable! {
    /// Overlays every device interface generation on one allocation, mirroring
    /// [`ID3D12GraphicsCommandListVtable`].
    pub(crate) union ID3D12DeviceVtable for ID3D12Device14_Vtbl;
}

pub(crate) struct ID3D12GraphicsCommandListHook<A: Vtable> {
    /// Hook-patched clone installed as the object's vtable pointer.
    patched: Box<ID3D12GraphicsCommandListVtable>,
    /// Verbatim copy of the original slots for downstream continuation.
    saved: ID3D12GraphicsCommandListVtable,
    /// The object's original vtable pointer, restored across releases.
    original: NonNull<c_void>,
    /// The widest interface generation cloned from the object's vtable.
    vtable: PhantomData<A>,
}

snowglass_hook::impl_com_hook! {
    impl CommandListHook for ID3D12GraphicsCommandListHook {
        vtable ID3D12GraphicsCommandListVtable;
        patch |patched| {
        // SAFETY: the slots written below were copied above, so each place
        // projection targets an initialized entry within the clone.
        unsafe {
            let unknown = patched.as_mut_ptr::<IUnknown_Vtbl>();
            (*unknown).Release = command_list_release;
            let table = patched.as_mut_ptr::<ID3D12GraphicsCommandList_Vtbl>();
            (*table).Reset = reset;
            (*table).RSSetViewports = set_viewports;
            (*table).RSSetScissorRects = set_scissors;
            (*table).DrawInstanced = draw_instanced;
            (*table).DrawIndexedInstanced = draw_indexed_instanced;
            (*table).ResourceBarrier = resource_barrier;
            (*table).SetGraphicsRootDescriptorTable = set_graphics_root_descriptor_table;
            (*table).SetGraphicsRootSignature = set_graphics_root_signature;
            (*table).OMSetRenderTargets = om_set_render_targets;
            if A::supports(TypeId::of::<ID3D12GraphicsCommandList7_Vtbl>()) {
                let table7 = patched.as_mut_ptr::<ID3D12GraphicsCommandList7_Vtbl>();
                (*table7).Barrier = enhanced_barrier;
            }
        }
        }

        #[vtable = IUnknown_Vtbl]
        #[base = Release]
        fn release(&self) -> ReleaseFn;
        #[vtable = ID3D12GraphicsCommandList_Vtbl]
        #[base = Reset]
        fn reset(&self) -> ResetFn;
        #[vtable = ID3D12GraphicsCommandList_Vtbl]
        #[base = RSSetViewports]
        fn set_viewports(&self) -> SetViewportsFn;
        #[vtable = ID3D12GraphicsCommandList_Vtbl]
        #[base = RSSetScissorRects]
        fn set_scissors(&self) -> SetScissorsFn;
        #[vtable = ID3D12GraphicsCommandList_Vtbl]
        #[base = DrawInstanced]
        fn draw_instanced(&self) -> DrawInstancedFn;
        #[vtable = ID3D12GraphicsCommandList_Vtbl]
        #[base = DrawIndexedInstanced]
        fn draw_indexed_instanced(&self) -> DrawIndexedInstancedFn;
        #[vtable = ID3D12GraphicsCommandList_Vtbl]
        #[base = ResourceBarrier]
        fn resource_barrier(&self) -> ResourceBarrierFn;
        #[vtable = ID3D12GraphicsCommandList_Vtbl]
        #[base = SetGraphicsRootDescriptorTable]
        fn set_graphics_root_descriptor_table(&self) -> SetGraphicsRootDescriptorTableFn;
        #[vtable = ID3D12GraphicsCommandList_Vtbl]
        #[base = SetGraphicsRootSignature]
        fn set_graphics_root_signature(&self) -> SetGraphicsRootSignatureFn;
        #[vtable = ID3D12GraphicsCommandList_Vtbl]
        #[base = OMSetRenderTargets]
        fn om_set_render_targets(&self) -> OmSetRenderTargetsFn;
        #[vtable = ID3D12GraphicsCommandList7_Vtbl]
        #[base = Barrier]
        fn barrier(&self) -> Option<EnhancedBarrierFn>;
    }
}

pub(crate) struct ID3D12DeviceHook<A: Vtable> {
    /// Hook-patched clone installed as the object's vtable pointer.
    patched: Box<ID3D12DeviceVtable>,
    /// Verbatim copy of the original slots for downstream continuation.
    saved: ID3D12DeviceVtable,
    /// The object's original vtable pointer, restored across releases.
    original: NonNull<c_void>,
    /// The widest interface generation cloned from the object's vtable.
    vtable: PhantomData<A>,
}

snowglass_hook::impl_com_hook! {
    impl DeviceHook for ID3D12DeviceHook {
        vtable ID3D12DeviceVtable;
        patch |patched| {
        // SAFETY: the slots written below were copied above, so each place
        // projection targets an initialized entry within the clone.
        unsafe {
            let unknown = patched.as_mut_ptr::<IUnknown_Vtbl>();
            (*unknown).Release = device_release;
            let table = patched.as_mut_ptr::<ID3D12Device_Vtbl>();
            (*table).CreateCommandList = create_command_list;
            (*table).CreateDescriptorHeap = create_descriptor_heap;
            (*table).CreateShaderResourceView = create_shader_resource_view;
            (*table).CreateRenderTargetView = create_render_target_view;
            (*table).CopyDescriptorsSimple = copy_descriptors_simple;
            if A::supports(TypeId::of::<ID3D12Device4_Vtbl>()) {
                let table4 = patched.as_mut_ptr::<ID3D12Device4_Vtbl>();
                (*table4).CreateCommandList1 = create_command_list1;
            }
        }
        }

        #[vtable = IUnknown_Vtbl]
        #[base = Release]
        fn release(&self) -> ReleaseFn;
        #[vtable = ID3D12Device_Vtbl]
        #[base = CreateCommandList]
        fn create_command_list(&self) -> CreateCommandListFn;
        #[vtable = ID3D12Device4_Vtbl]
        #[base = CreateCommandList1]
        fn create_command_list1(&self) -> Option<CreateCommandList1Fn>;
        #[vtable = ID3D12Device_Vtbl]
        #[base = CreateDescriptorHeap]
        fn create_descriptor_heap(&self) -> CreateDescriptorHeapFn;
        #[vtable = ID3D12Device_Vtbl]
        #[base = CreateShaderResourceView]
        fn create_shader_resource_view(&self) -> CreateShaderResourceViewFn;
        #[vtable = ID3D12Device_Vtbl]
        #[base = CreateRenderTargetView]
        fn create_render_target_view(&self) -> CreateRenderTargetViewFn;
        #[vtable = ID3D12Device_Vtbl]
        #[base = CopyDescriptorsSimple]
        fn copy_descriptors_simple(&self) -> CopyDescriptorsSimpleFn;
    }
}

/// Per-interface hook state seeded by `CreateDeviceContext`.
struct DeviceContext {
    hook: Box<dyn DeviceHook>,
    tracking: Arc<snowglass_producer_d3d12::Tracking>,
}

/// Per-interface hook state inherited from the creating device context.
struct CommandListContext {
    hook: Box<dyn CommandListHook>,
    tracking: Arc<snowglass_producer_d3d12::Tracking>,
}

static DEVICES: LazyLock<DashMap<usize, DeviceContext>> = LazyLock::new(Default::default);
static COMMAND_LISTS: LazyLock<DashMap<usize, CommandListContext>> =
    LazyLock::new(Default::default);

/// A monomorphized per-object install routine, used to dispatch a widest-cast
/// result to a typed layout.
type InstallFn = unsafe fn(*mut c_void, Arc<snowglass_producer_d3d12::Tracking>) -> bool;

/// Installs render-target tracking on a device returned by
/// `D3D12CreateDevice`. Command lists are hooked as the device creates them.
///
/// # Safety
///
/// `output` must be null or point to a COM interface pointer for a live
/// `ID3D12Device`.
pub(crate) unsafe fn install_device(
    output: *mut *mut c_void,
    tracking: Arc<snowglass_producer_d3d12::Tracking>,
) {
    if !snowglass_hook::config::tracking_enabled() || output.is_null() {
        return;
    }
    match unsafe { try_install_device(output, tracking) } {
        Ok(true) => crate::d3d12_log("snowglass: installed D3D12 viewport/scissor tracking"),
        Ok(false) => {}
        Err(error) => crate::d3d12_log(format!(
            "snowglass: failed to install D3D12 viewport/scissor tracking: {error}"
        )),
    }
}

unsafe fn try_install_device(
    output: *mut *mut c_void,
    tracking: Arc<snowglass_producer_d3d12::Tracking>,
) -> Result<bool, String> {
    let raw = unsafe { *output };
    let unknown = unsafe { IUnknown::from_raw_borrowed(&raw) }
        .ok_or_else(|| "D3D12CreateDevice returned a null device".to_owned())?;
    let device: ID3D12Device = unknown.cast().map_err(|error| error.to_string())?;
    if let Some(enhanced_barriers) = tracking.detect_device_features(&device)? {
        crate::d3d12_log(if enhanced_barriers {
            "snowglass: D3D12 enhanced capture barriers enabled"
        } else {
            "snowglass: D3D12 capture using legacy resource barriers"
        });
    }
    let base = device.as_raw();
    let already_hooked = DEVICES.get(&(base as usize)).is_some_and(|context| unsafe {
        *base.cast::<*const usize>() == context.hook.patched_vtable()
    });

    let interfaces = device_interfaces(&device);
    let mut installed = false;
    for (interface, install) in interfaces {
        installed |= unsafe { install(interface, tracking.clone()) };
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

unsafe fn install_device_kind<A: Vtable>(
    device: *mut c_void,
    tracking: Arc<snowglass_producer_d3d12::Tracking>,
) -> bool {
    if device.is_null() {
        return false;
    }
    let entry = DEVICES.entry(device as usize);
    let current = unsafe { *device.cast::<*const usize>() };
    let vtable = match &entry {
        Entry::Occupied(occupied) => {
            let existing = &occupied.get().hook;
            if current == existing.patched_vtable() {
                if A::supported_by(existing.vtable_type()) {
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
    let hook: Box<dyn DeviceHook> = Box::new(vtable);
    unsafe { *device.cast::<*const usize>() = hook.patched_vtable() };
    entry.insert(DeviceContext { hook, tracking });
    true
}

/// Installs tracking interception on a command list created by a hooked
/// device.
///
/// # Safety
///
/// `list` must be null or point to a live COM command-list interface.
unsafe fn install_command_list(
    list: *mut c_void,
    interface_id: *const GUID,
    tracking: Arc<snowglass_producer_d3d12::Tracking>,
) {
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
        unsafe { install(pointer, tracking.clone()) };
    }
}

unsafe fn install_command_list_kind<A: Vtable>(
    list: *mut c_void,
    tracking: Arc<snowglass_producer_d3d12::Tracking>,
) -> bool {
    if list.is_null() {
        return false;
    }
    let entry = COMMAND_LISTS.entry(list as usize);
    let current = unsafe { *list.cast::<*const usize>() };
    let vtable = match &entry {
        Entry::Occupied(occupied) => {
            let existing = &occupied.get().hook;
            if current == existing.patched_vtable() {
                if A::supported_by(existing.vtable_type()) {
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
    let hook: Box<dyn CommandListHook> = Box::new(vtable);
    unsafe { *list.cast::<*const usize>() = hook.patched_vtable() };
    entry.insert(CommandListContext { hook, tracking });
    true
}

fn device_method<T>(device: *mut c_void, select: impl FnOnce(&dyn DeviceHook) -> T) -> Option<T> {
    DEVICES
        .get(&(device as usize))
        .map(|context| select(&*context.hook))
}

fn list_method<T>(list: *mut c_void, select: impl FnOnce(&dyn CommandListHook) -> T) -> Option<T> {
    COMMAND_LISTS
        .get(&(list as usize))
        .map(|context| select(&*context.hook))
}

fn device_tracking(device: *mut c_void) -> Option<Arc<snowglass_producer_d3d12::Tracking>> {
    DEVICES
        .get(&(device as usize))
        .map(|context| context.tracking.clone())
}

pub(crate) fn list_tracking(list: *mut c_void) -> Option<Arc<snowglass_producer_d3d12::Tracking>> {
    COMMAND_LISTS
        .get(&(list as usize))
        .map(|context| context.tracking.clone())
}

/// Resolves the tracking context associated with a D3D12 command queue's device.
///
/// # Safety
///
/// `queue` must be null or point to a live `ID3D12CommandQueue`.
pub(crate) unsafe fn queue_tracking(
    queue: *mut c_void,
) -> Option<Arc<snowglass_producer_d3d12::Tracking>> {
    let queue = unsafe { ID3D12CommandQueue::from_raw_borrowed(&queue) }?;
    let mut device = None;
    unsafe { queue.GetDevice(&mut device).ok()? };
    let device: ID3D12Device = device?;
    device_tracking(device.as_raw())
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
        if let Some(tracking) = device_tracking(this) {
            unsafe { install_command_list(*output, interface_id, tracking) };
        }
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
        if let Some(tracking) = device_tracking(this) {
            unsafe { install_command_list(*output, interface_id, tracking) };
        }
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
        if let Some(tracking) = device_tracking(this) {
            unsafe { tracking.record_descriptor_heap(desc, output) };
        }
    }
    result
}

snowglass_hook::com_trampoline! {
    unsafe extern "system" fn create_shader_resource_view(
        this: *mut c_void,
        resource: *mut c_void,
        desc: *const D3D12_SHADER_RESOURCE_VIEW_DESC,
        destination: D3D12_CPU_DESCRIPTOR_HANDLE,
    );
    original device_method(this, |hook| hook.create_shader_resource_view());
    after {
        if let Some(tracking) = device_tracking(this) {
            unsafe { tracking.record_shader_resource_view(resource, desc, destination) };
        }
    }
}

snowglass_hook::com_trampoline! {
    unsafe extern "system" fn create_render_target_view(
        this: *mut c_void,
        resource: *mut c_void,
        desc: *const D3D12_RENDER_TARGET_VIEW_DESC,
        destination: D3D12_CPU_DESCRIPTOR_HANDLE,
    );
    original device_method(this, |hook| hook.create_render_target_view());
    after {
        if let Some(tracking) = device_tracking(this) {
            unsafe { tracking.record_render_target_view(resource, desc, destination) };
        }
    }
}

snowglass_hook::com_trampoline! {
    unsafe extern "system" fn copy_descriptors_simple(
        this: *mut c_void,
        count: u32,
        destination: D3D12_CPU_DESCRIPTOR_HANDLE,
        source: D3D12_CPU_DESCRIPTOR_HANDLE,
        heap_type: D3D12_DESCRIPTOR_HEAP_TYPE,
    );
    original device_method(this, |hook| hook.copy_descriptors_simple());
    after {
        if let Some(tracking) = device_tracking(this) {
            tracking.record_copy_descriptors_simple(count, destination, source, heap_type);
        }
    }
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
        if let Some(tracking) = list_tracking(this) {
            tracking.command_reset(this);
        }
    }
    result
}

snowglass_hook::com_trampoline! {
    unsafe extern "system" fn set_viewports(
        this: *mut c_void,
        count: u32,
        viewports: *const D3D12_VIEWPORT,
    );
    original list_method(this, |hook| hook.set_viewports());
    after {
        if let Some(tracking) = list_tracking(this) {
            unsafe { tracking.record_viewports(this, count, viewports) };
        }
    }
}

snowglass_hook::com_trampoline! {
    unsafe extern "system" fn set_scissors(
        this: *mut c_void,
        count: u32,
        scissors: *const RECT,
    );
    original list_method(this, |hook| hook.set_scissors());
    after {
        if let Some(tracking) = list_tracking(this) {
            unsafe { tracking.record_scissors(this, count, scissors) };
        }
    }
}

snowglass_hook::com_trampoline! {
    unsafe extern "system" fn draw_instanced(
        this: *mut c_void,
        vertex_count: u32,
        instance_count: u32,
        start_vertex: u32,
        start_instance: u32,
    );
    original list_method(this, |hook| hook.draw_instanced());
    after {
        if let Some(tracking) = list_tracking(this) {
            tracking.record_draw(this);
        }
    }
}

snowglass_hook::com_trampoline! {
    unsafe extern "system" fn draw_indexed_instanced(
        this: *mut c_void,
        index_count: u32,
        instance_count: u32,
        start_index: u32,
        base_vertex: i32,
        start_instance: u32,
    );
    original list_method(this, |hook| hook.draw_indexed_instanced());
    after {
        if let Some(tracking) = list_tracking(this) {
            tracking.record_draw(this);
        }
    }
}

snowglass_hook::com_trampoline! {
    unsafe extern "system" fn resource_barrier(
        this: *mut c_void,
        count: u32,
        barriers: *const D3D12_RESOURCE_BARRIER,
    );
    original list_method(this, |hook| hook.resource_barrier());
    after {
        if let Some(tracking) = list_tracking(this) {
            unsafe { tracking.record_resource_barrier(this, count, barriers) };
        }
    }
}

snowglass_hook::com_trampoline! {
    unsafe extern "system" fn set_graphics_root_descriptor_table(
        this: *mut c_void,
        root_index: u32,
        descriptor: D3D12_GPU_DESCRIPTOR_HANDLE,
    );
    original list_method(this, |hook| hook.set_graphics_root_descriptor_table());
    after {
        if let Some(tracking) = list_tracking(this) {
            tracking.record_root_descriptor_table(this, root_index, descriptor);
        }
    }
}

snowglass_hook::com_trampoline! {
    unsafe extern "system" fn set_graphics_root_signature(
        this: *mut c_void,
        signature: *mut c_void,
    );
    original list_method(this, |hook| hook.set_graphics_root_signature());
    after {
        if let Some(tracking) = list_tracking(this) {
            tracking.record_root_signature(this);
        }
    }
}

snowglass_hook::com_trampoline! {
    unsafe extern "system" fn om_set_render_targets(
        this: *mut c_void,
        count: u32,
        descriptors: *const D3D12_CPU_DESCRIPTOR_HANDLE,
        contiguous: BOOL,
        depth: *const D3D12_CPU_DESCRIPTOR_HANDLE,
    );
    original list_method(this, |hook| hook.om_set_render_targets());
    after {
        if let Some(tracking) = list_tracking(this) {
            unsafe { tracking.record_render_targets(this, count, descriptors) };
        }
    }
}

snowglass_hook::com_trampoline! {
    unsafe extern "system" fn enhanced_barrier(
        this: *mut c_void,
        count: u32,
        groups: *const D3D12_BARRIER_GROUP,
    );
    original list_method(this, |hook| hook.barrier()).flatten();
    after {
        if let Some(tracking) = list_tracking(this) {
            unsafe { tracking.record_enhanced_barrier(this, count, groups) };
        }
    }
}
