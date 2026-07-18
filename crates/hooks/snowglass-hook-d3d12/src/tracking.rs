//! D3D12 command-list and device method interception that feeds capture
//! tracking.
//!
//! Each device returned by `D3D12CreateDevice` — and each graphics command
//! list it creates — receives a per-object typed clone of its vtable with the
//! tracked slots redirected, matching the DXGI and DirectInput hook handling.
//! The trampolines forward to the original method and then hand the observed
//! state to the producer's tracking module, which owns all capture-region
//! bookkeeping. This module keeps only the interception plumbing: the hooked
//! object registries and the device-setup feature detection.

use crate::vtable::{
    DeviceKind, GraphicsCommandListKind, ID3D12DeviceHook, ID3D12GraphicsCommandListHook,
};
use dashmap::{DashMap, mapref::entry::Entry};
use std::{
    ffi::c_void,
    sync::{LazyLock, OnceLock},
};
use windows::{
    Win32::{
        Foundation::{E_FAIL, RECT},
        Graphics::Direct3D12::{
            D3D12_BARRIER_GROUP, D3D12_COMMAND_LIST_FLAGS, D3D12_COMMAND_LIST_TYPE,
            D3D12_COMMAND_LIST_TYPE_DIRECT, D3D12_CPU_DESCRIPTOR_HANDLE,
            D3D12_DESCRIPTOR_HEAP_DESC, D3D12_DESCRIPTOR_HEAP_TYPE,
            D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV, D3D12_FEATURE_D3D12_OPTIONS12,
            D3D12_FEATURE_DATA_D3D12_OPTIONS12, D3D12_GPU_DESCRIPTOR_HANDLE,
            D3D12_RENDER_TARGET_VIEW_DESC, D3D12_RESOURCE_BARRIER, D3D12_SHADER_RESOURCE_VIEW_DESC,
            D3D12_VIEWPORT, ID3D12CommandAllocator, ID3D12Device, ID3D12Device1, ID3D12Device2,
            ID3D12Device3, ID3D12Device4, ID3D12Device5, ID3D12Device6, ID3D12Device7,
            ID3D12Device8, ID3D12Device9, ID3D12Device10, ID3D12Device11, ID3D12Device12,
            ID3D12Device13, ID3D12Device14, ID3D12GraphicsCommandList, ID3D12GraphicsCommandList1,
            ID3D12GraphicsCommandList2, ID3D12GraphicsCommandList3, ID3D12GraphicsCommandList4,
            ID3D12GraphicsCommandList5, ID3D12GraphicsCommandList6, ID3D12GraphicsCommandList7,
            ID3D12GraphicsCommandList8, ID3D12GraphicsCommandList9, ID3D12GraphicsCommandList10,
            ID3D12PipelineState,
        },
    },
    core::{BOOL, GUID, HRESULT, IUnknown, Interface},
};

static DEVICES: LazyLock<DashMap<usize, ID3D12DeviceHook>> = LazyLock::new(Default::default);
static COMMAND_LISTS: LazyLock<DashMap<usize, ID3D12GraphicsCommandListHook>> =
    LazyLock::new(Default::default);
static FEATURES_PROBED: OnceLock<()> = OnceLock::new();

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
    detect_device_features(&device)?;
    let base = device.as_raw();
    let already_hooked = DEVICES
        .get(&(base as usize))
        .is_some_and(|hook| unsafe { *base.cast::<*const usize>() } == hook.patched_vtable());
    let extended = widest_device(&device);
    let mut installed = unsafe { install_device_kind(base, DeviceKind::Device) };
    if let Some((extended, kind)) = extended {
        installed |= unsafe { install_device_kind(extended, kind) };
    }
    if !installed {
        return Err("could not clone the device vtable".to_owned());
    }
    Ok(!already_hooked)
}

/// Detects capture-related device features once per process: whether enhanced
/// barriers can be recorded, and the shader-visible descriptor stride.
fn detect_device_features(device: &ID3D12Device) -> Result<(), String> {
    if FEATURES_PROBED.get().is_some() {
        return Ok(());
    }
    // A throwaway command list probes whether the runtime implements the
    // enhanced-barrier interface generation.
    let list: ID3D12GraphicsCommandList = unsafe {
        let allocator: ID3D12CommandAllocator = device
            .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)
            .map_err(|error| error.to_string())?;
        let list: ID3D12GraphicsCommandList = device
            .CreateCommandList(
                0,
                D3D12_COMMAND_LIST_TYPE_DIRECT,
                &allocator,
                None::<&ID3D12PipelineState>,
            )
            .map_err(|error| error.to_string())?;
        list.Close().map_err(|error| error.to_string())?;
        list
    };
    let mut options12 = D3D12_FEATURE_DATA_D3D12_OPTIONS12::default();
    let enhanced_barriers_advertised = unsafe {
        device
            .CheckFeatureSupport(
                D3D12_FEATURE_D3D12_OPTIONS12,
                (&raw mut options12).cast(),
                std::mem::size_of_val(&options12) as u32,
            )
            .is_ok()
            && options12.EnhancedBarriersSupported.as_bool()
    };
    let enhanced_barriers_supported =
        enhanced_barriers_advertised && list.cast::<ID3D12GraphicsCommandList7>().is_ok();
    snowglass_producer_d3d12::set_enhanced_barriers_supported(enhanced_barriers_supported);
    crate::d3d12_log(if enhanced_barriers_supported {
        "snowglass: D3D12 enhanced capture barriers enabled"
    } else {
        "snowglass: D3D12 capture using legacy resource barriers"
    });
    snowglass_producer_d3d12::set_descriptor_increment(unsafe {
        device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV)
    });
    let _ = FEATURES_PROBED.set(());
    Ok(())
}

fn widest_device(device: &ID3D12Device) -> Option<(*mut c_void, DeviceKind)> {
    macro_rules! try_cast {
        ($interface:ty, $kind:expr) => {
            if let Ok(interface) = device.cast::<$interface>() {
                return Some((interface.as_raw(), $kind));
            }
        };
    }
    try_cast!(ID3D12Device14, DeviceKind::Device14);
    try_cast!(ID3D12Device13, DeviceKind::Device13);
    try_cast!(ID3D12Device12, DeviceKind::Device12);
    try_cast!(ID3D12Device11, DeviceKind::Device11);
    try_cast!(ID3D12Device10, DeviceKind::Device10);
    try_cast!(ID3D12Device9, DeviceKind::Device9);
    try_cast!(ID3D12Device8, DeviceKind::Device8);
    try_cast!(ID3D12Device7, DeviceKind::Device7);
    try_cast!(ID3D12Device6, DeviceKind::Device6);
    try_cast!(ID3D12Device5, DeviceKind::Device5);
    try_cast!(ID3D12Device4, DeviceKind::Device4);
    try_cast!(ID3D12Device3, DeviceKind::Device3);
    try_cast!(ID3D12Device2, DeviceKind::Device2);
    try_cast!(ID3D12Device1, DeviceKind::Device1);
    None
}

fn widest_command_list(
    list: &ID3D12GraphicsCommandList,
) -> Option<(*mut c_void, GraphicsCommandListKind)> {
    macro_rules! try_cast {
        ($interface:ty, $kind:expr) => {
            if let Ok(interface) = list.cast::<$interface>() {
                return Some((interface.as_raw(), $kind));
            }
        };
    }
    try_cast!(ID3D12GraphicsCommandList10, GraphicsCommandListKind::List10);
    try_cast!(ID3D12GraphicsCommandList9, GraphicsCommandListKind::List9);
    try_cast!(ID3D12GraphicsCommandList8, GraphicsCommandListKind::List8);
    try_cast!(ID3D12GraphicsCommandList7, GraphicsCommandListKind::List7);
    try_cast!(ID3D12GraphicsCommandList6, GraphicsCommandListKind::List6);
    try_cast!(ID3D12GraphicsCommandList5, GraphicsCommandListKind::List5);
    try_cast!(ID3D12GraphicsCommandList4, GraphicsCommandListKind::List4);
    try_cast!(ID3D12GraphicsCommandList3, GraphicsCommandListKind::List3);
    try_cast!(ID3D12GraphicsCommandList2, GraphicsCommandListKind::List2);
    try_cast!(ID3D12GraphicsCommandList1, GraphicsCommandListKind::List1);
    None
}

unsafe fn install_device_kind(device: *mut c_void, kind: DeviceKind) -> bool {
    if device.is_null() {
        return false;
    }
    let entry = DEVICES.entry(device as usize);
    let current = unsafe { *device.cast::<*const usize>() };
    let vtable = match &entry {
        Entry::Occupied(occupied) => {
            let existing = occupied.get();
            if current == existing.patched_vtable() {
                if existing.kind() >= kind {
                    return true;
                }
                unsafe { ID3D12DeviceHook::clone_from(existing.original_vtable(), kind) }
            } else {
                unsafe { ID3D12DeviceHook::clone_from(current, kind) }
            }
        }
        Entry::Vacant(_) => unsafe { ID3D12DeviceHook::clone_from(current, kind) },
    };
    let Some(vtable) = vtable else {
        return false;
    };
    unsafe { *device.cast::<*const usize>() = vtable.patched_vtable() };
    entry.insert(vtable);
    true
}

/// Installs tracking interception on a command list created by a hooked
/// device.
///
/// # Safety
///
/// `list` must be null or point to a live COM command-list interface.
unsafe fn install_command_list(list: *mut c_void) {
    if list.is_null() {
        return;
    }
    let Some(unknown) = (unsafe { IUnknown::from_raw_borrowed(&list) }) else {
        return;
    };
    // Video and other non-graphics list kinds carry none of the tracked
    // methods.
    let Ok(base) = unknown.cast::<ID3D12GraphicsCommandList>() else {
        return;
    };
    let extended = widest_command_list(&base);
    unsafe { install_command_list_kind(base.as_raw(), GraphicsCommandListKind::List) };
    if let Some((extended, kind)) = extended {
        unsafe { install_command_list_kind(extended, kind) };
    }
}

unsafe fn install_command_list_kind(list: *mut c_void, kind: GraphicsCommandListKind) -> bool {
    if list.is_null() {
        return false;
    }
    let entry = COMMAND_LISTS.entry(list as usize);
    let current = unsafe { *list.cast::<*const usize>() };
    let vtable = match &entry {
        Entry::Occupied(occupied) => {
            let existing = occupied.get();
            if current == existing.patched_vtable() {
                if existing.kind() >= kind {
                    return true;
                }
                unsafe {
                    ID3D12GraphicsCommandListHook::clone_from(existing.original_vtable(), kind)
                }
            } else {
                unsafe { ID3D12GraphicsCommandListHook::clone_from(current, kind) }
            }
        }
        Entry::Vacant(_) => unsafe { ID3D12GraphicsCommandListHook::clone_from(current, kind) },
    };
    let Some(vtable) = vtable else {
        return false;
    };
    unsafe { *list.cast::<*const usize>() = vtable.patched_vtable() };
    entry.insert(vtable);
    true
}

fn device_method<T>(
    device: *mut c_void,
    select: impl FnOnce(&ID3D12DeviceHook) -> T,
) -> Option<T> {
    DEVICES.get(&(device as usize)).map(|hook| select(&hook))
}

fn list_method<T>(
    list: *mut c_void,
    select: impl FnOnce(&ID3D12GraphicsCommandListHook) -> T,
) -> Option<T> {
    COMMAND_LISTS.get(&(list as usize)).map(|hook| select(&hook))
}

pub(crate) unsafe extern "system" fn device_release(this: *mut c_void) -> u32 {
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
    } else if let Some(patched) = device_method(this, ID3D12DeviceHook::patched_vtable) {
        unsafe { *this.cast::<*const usize>() = patched };
    }
    references
}

pub(crate) unsafe extern "system" fn command_list_release(this: *mut c_void) -> u32 {
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
    } else if let Some(patched) = list_method(this, ID3D12GraphicsCommandListHook::patched_vtable) {
        unsafe { *this.cast::<*const usize>() = patched };
    }
    references
}

pub(crate) unsafe extern "system" fn create_command_list(
    this: *mut c_void,
    node_mask: u32,
    list_type: D3D12_COMMAND_LIST_TYPE,
    allocator: *mut c_void,
    initial_state: *mut c_void,
    interface_id: *const GUID,
    output: *mut *mut c_void,
) -> HRESULT {
    let Some(original) = device_method(this, ID3D12DeviceHook::create_command_list) else {
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
        unsafe { install_command_list(*output) };
    }
    result
}

pub(crate) unsafe extern "system" fn create_command_list1(
    this: *mut c_void,
    node_mask: u32,
    list_type: D3D12_COMMAND_LIST_TYPE,
    flags: D3D12_COMMAND_LIST_FLAGS,
    interface_id: *const GUID,
    output: *mut *mut c_void,
) -> HRESULT {
    let Some(original) = device_method(this, ID3D12DeviceHook::create_command_list1).flatten()
    else {
        return E_FAIL;
    };
    let result = unsafe { original(this, node_mask, list_type, flags, interface_id, output) };
    if result.is_ok() && !output.is_null() {
        unsafe { install_command_list(*output) };
    }
    result
}

pub(crate) unsafe extern "system" fn create_descriptor_heap(
    this: *mut c_void,
    desc: *const D3D12_DESCRIPTOR_HEAP_DESC,
    interface_id: *const GUID,
    output: *mut *mut c_void,
) -> HRESULT {
    let Some(original) = device_method(this, ID3D12DeviceHook::create_descriptor_heap) else {
        return E_FAIL;
    };
    let result = unsafe { original(this, desc, interface_id, output) };
    if result.is_ok() {
        unsafe { snowglass_producer_d3d12::record_descriptor_heap(desc, output) };
    }
    result
}

pub(crate) unsafe extern "system" fn create_shader_resource_view(
    this: *mut c_void,
    resource: *mut c_void,
    desc: *const D3D12_SHADER_RESOURCE_VIEW_DESC,
    destination: D3D12_CPU_DESCRIPTOR_HANDLE,
) {
    if let Some(original) = device_method(this, ID3D12DeviceHook::create_shader_resource_view) {
        unsafe { original(this, resource, desc, destination) };
    }
    unsafe { snowglass_producer_d3d12::record_shader_resource_view(resource, desc, destination) };
}

pub(crate) unsafe extern "system" fn create_render_target_view(
    this: *mut c_void,
    resource: *mut c_void,
    desc: *const D3D12_RENDER_TARGET_VIEW_DESC,
    destination: D3D12_CPU_DESCRIPTOR_HANDLE,
) {
    if let Some(original) = device_method(this, ID3D12DeviceHook::create_render_target_view) {
        unsafe { original(this, resource, desc, destination) };
    }
    unsafe { snowglass_producer_d3d12::record_render_target_view(resource, desc, destination) };
}

pub(crate) unsafe extern "system" fn copy_descriptors_simple(
    this: *mut c_void,
    count: u32,
    destination: D3D12_CPU_DESCRIPTOR_HANDLE,
    source: D3D12_CPU_DESCRIPTOR_HANDLE,
    heap_type: D3D12_DESCRIPTOR_HEAP_TYPE,
) {
    if let Some(original) = device_method(this, ID3D12DeviceHook::copy_descriptors_simple) {
        unsafe { original(this, count, destination, source, heap_type) };
    }
    snowglass_producer_d3d12::record_copy_descriptors_simple(count, destination, source, heap_type);
}

pub(crate) unsafe extern "system" fn reset(
    this: *mut c_void,
    allocator: *mut c_void,
    initial_state: *mut c_void,
) -> HRESULT {
    let Some(original) = list_method(this, ID3D12GraphicsCommandListHook::reset) else {
        return E_FAIL;
    };
    let result = unsafe { original(this, allocator, initial_state) };
    if result.is_ok() {
        snowglass_producer_d3d12::command_reset(this);
    }
    result
}

pub(crate) unsafe extern "system" fn set_viewports(
    this: *mut c_void,
    count: u32,
    viewports: *const D3D12_VIEWPORT,
) {
    if let Some(original) = list_method(this, ID3D12GraphicsCommandListHook::set_viewports) {
        unsafe { original(this, count, viewports) };
    }
    unsafe { snowglass_producer_d3d12::record_viewports(this, count, viewports) };
}

pub(crate) unsafe extern "system" fn set_scissors(
    this: *mut c_void,
    count: u32,
    scissors: *const RECT,
) {
    if let Some(original) = list_method(this, ID3D12GraphicsCommandListHook::set_scissors) {
        unsafe { original(this, count, scissors) };
    }
    unsafe { snowglass_producer_d3d12::record_scissors(this, count, scissors) };
}

pub(crate) unsafe extern "system" fn draw_instanced(
    this: *mut c_void,
    vertex_count: u32,
    instance_count: u32,
    start_vertex: u32,
    start_instance: u32,
) {
    if let Some(original) = list_method(this, ID3D12GraphicsCommandListHook::draw_instanced) {
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

pub(crate) unsafe extern "system" fn draw_indexed_instanced(
    this: *mut c_void,
    index_count: u32,
    instance_count: u32,
    start_index: u32,
    base_vertex: i32,
    start_instance: u32,
) {
    if let Some(original) = list_method(this, ID3D12GraphicsCommandListHook::draw_indexed_instanced)
    {
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

pub(crate) unsafe extern "system" fn resource_barrier(
    this: *mut c_void,
    count: u32,
    barriers: *const D3D12_RESOURCE_BARRIER,
) {
    if let Some(original) = list_method(this, ID3D12GraphicsCommandListHook::resource_barrier) {
        unsafe { original(this, count, barriers) };
    }
    unsafe { snowglass_producer_d3d12::record_resource_barrier(this, count, barriers) };
}

pub(crate) unsafe extern "system" fn set_graphics_root_descriptor_table(
    this: *mut c_void,
    root_index: u32,
    descriptor: D3D12_GPU_DESCRIPTOR_HANDLE,
) {
    if let Some(original) = list_method(
        this,
        ID3D12GraphicsCommandListHook::set_graphics_root_descriptor_table,
    ) {
        unsafe { original(this, root_index, descriptor) };
    }
    snowglass_producer_d3d12::record_root_descriptor_table(this, root_index, descriptor);
}

pub(crate) unsafe extern "system" fn set_graphics_root_signature(
    this: *mut c_void,
    signature: *mut c_void,
) {
    if let Some(original) = list_method(
        this,
        ID3D12GraphicsCommandListHook::set_graphics_root_signature,
    ) {
        unsafe { original(this, signature) };
    }
    snowglass_producer_d3d12::record_root_signature(this);
}

pub(crate) unsafe extern "system" fn om_set_render_targets(
    this: *mut c_void,
    count: u32,
    descriptors: *const D3D12_CPU_DESCRIPTOR_HANDLE,
    contiguous: BOOL,
    depth: *const D3D12_CPU_DESCRIPTOR_HANDLE,
) {
    if let Some(original) = list_method(this, ID3D12GraphicsCommandListHook::om_set_render_targets)
    {
        unsafe { original(this, count, descriptors, contiguous, depth) };
    }
    unsafe { snowglass_producer_d3d12::record_render_targets(this, count, descriptors) };
}

pub(crate) unsafe extern "system" fn enhanced_barrier(
    this: *mut c_void,
    count: u32,
    groups: *const D3D12_BARRIER_GROUP,
) {
    if let Some(original) = list_method(this, ID3D12GraphicsCommandListHook::barrier).flatten() {
        unsafe { original(this, count, groups) };
    }
    unsafe { snowglass_producer_d3d12::record_enhanced_barrier(this, count, groups) };
}
