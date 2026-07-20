//! Typed clones of D3D12 device and graphics command-list vtables with
//! interception applied.
//!
//! Each device returned by `D3D12CreateDevice` — and each graphics command
//! list it creates — receives a per-object heap clone of its vtable with the
//! tracked slots redirected, matching the DXGI and DirectInput hook handling.
//! The clone is copied as opaque words (a vtable may legally carry null or
//! reserved slots, so whole-struct reads would assert validity the source
//! cannot guarantee), while individual slots are read and patched through the
//! `windows` crate's vtable definitions overlaid by a union. The hook chains
//! forward to the base method and then hand the observed state to the
//! producer's tracking module, which owns all capture-region bookkeeping.

use std::ffi::c_void;
use std::sync::Arc;
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
    core::{BOOL, GUID, HRESULT, IUnknown, Interface},
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

snowglass_hook::impl_com_hook! {
    pub(crate) registry COMMAND_LISTS;
    pub(crate) trait CommandListHook;
    impl CommandListHook for ID3D12GraphicsCommandListHook {
        vtable ID3D12GraphicsCommandListVtable;
        #[release]
        unsafe extern "system" fn command_list_release(this: *mut c_void) -> u32 {
            context struct {
                tracking: Arc<snowglass_producer_d3d12::Tracking>,
            }
        }

        #[vtable = ID3D12GraphicsCommandList_Vtbl]
        #[base = Reset]
        #[fallback = E_FAIL]
        #[infix(track_reset)]
        unsafe extern "system" fn reset(this: *mut c_void, allocator: *mut c_void, initial_state: *mut c_void) -> HRESULT {
            context |state| {
                tracking: Arc<snowglass_producer_d3d12::Tracking> = state.tracking().clone(),
                patched_vtable: *const usize = state.patched_vtable(),
            }
        }

        #[vtable = ID3D12GraphicsCommandList_Vtbl]
        #[base = RSSetViewports]
        #[postfix(record_viewports(this, count, viewports))]
        unsafe extern "system" fn set_viewports(this: *mut c_void, count: u32, viewports: *const D3D12_VIEWPORT) {
            context |state| {
                tracking: Arc<snowglass_producer_d3d12::Tracking> = state.tracking().clone(),
            }
        }

        #[vtable = ID3D12GraphicsCommandList_Vtbl]
        #[base = RSSetScissorRects]
        #[postfix(record_scissors(this, count, scissors))]
        unsafe extern "system" fn set_scissors(this: *mut c_void, count: u32, scissors: *const RECT) {
            context |state| {
                tracking: Arc<snowglass_producer_d3d12::Tracking> = state.tracking().clone(),
            }
        }

        #[vtable = ID3D12GraphicsCommandList_Vtbl]
        #[base = DrawInstanced]
        #[postfix(record_draw_instanced(this, vertex_count, instance_count, start_vertex, start_instance))]
        unsafe extern "system" fn draw_instanced(this: *mut c_void, vertex_count: u32, instance_count: u32, start_vertex: u32, start_instance: u32) {
            context |state| {
                tracking: Arc<snowglass_producer_d3d12::Tracking> = state.tracking().clone(),
            }
        }

        #[vtable = ID3D12GraphicsCommandList_Vtbl]
        #[base = DrawIndexedInstanced]
        #[postfix(record_draw_indexed_instanced(this, index_count, instance_count, start_index, base_vertex, start_instance))]
        unsafe extern "system" fn draw_indexed_instanced(this: *mut c_void, index_count: u32, instance_count: u32, start_index: u32, base_vertex: i32, start_instance: u32) {
            context |state| {
                tracking: Arc<snowglass_producer_d3d12::Tracking> = state.tracking().clone(),
            }
        }

        #[vtable = ID3D12GraphicsCommandList_Vtbl]
        #[base = ResourceBarrier]
        #[postfix(record_resource_barrier(this, count, barriers))]
        unsafe extern "system" fn resource_barrier(this: *mut c_void, count: u32, barriers: *const D3D12_RESOURCE_BARRIER) {
            context |state| {
                tracking: Arc<snowglass_producer_d3d12::Tracking> = state.tracking().clone(),
            }
        }

        #[vtable = ID3D12GraphicsCommandList_Vtbl]
        #[base = SetGraphicsRootDescriptorTable]
        #[postfix(record_root_descriptor_table(this, root_index, descriptor))]
        unsafe extern "system" fn set_graphics_root_descriptor_table(this: *mut c_void, root_index: u32, descriptor: D3D12_GPU_DESCRIPTOR_HANDLE) {
            context |state| {
                tracking: Arc<snowglass_producer_d3d12::Tracking> = state.tracking().clone(),
            }
        }

        #[vtable = ID3D12GraphicsCommandList_Vtbl]
        #[base = SetGraphicsRootSignature]
        #[postfix(record_root_signature(this, signature))]
        unsafe extern "system" fn set_graphics_root_signature(this: *mut c_void, signature: *mut c_void) {
            context |state| {
                tracking: Arc<snowglass_producer_d3d12::Tracking> = state.tracking().clone(),
            }
        }

        #[vtable = ID3D12GraphicsCommandList_Vtbl]
        #[base = OMSetRenderTargets]
        #[postfix(record_render_targets(this, count, descriptors, contiguous, depth))]
        unsafe extern "system" fn om_set_render_targets(this: *mut c_void, count: u32, descriptors: *const D3D12_CPU_DESCRIPTOR_HANDLE, contiguous: BOOL, depth: *const D3D12_CPU_DESCRIPTOR_HANDLE) {
            context |state| {
                tracking: Arc<snowglass_producer_d3d12::Tracking> = state.tracking().clone(),
            }
        }

        #[vtable = ID3D12GraphicsCommandList7_Vtbl]
        #[base = Barrier]
        #[postfix(record_enhanced_barrier(this, count, groups))]
        unsafe extern "system" fn enhanced_barrier(this: *mut c_void, count: u32, groups: *const D3D12_BARRIER_GROUP) {
            context |state| {
                tracking: Arc<snowglass_producer_d3d12::Tracking> = state.tracking().clone(),
            }
        }
    }
}

snowglass_hook::impl_com_hook! {
    pub(crate) registry DEVICES;
    pub(crate) trait DeviceHook;
    impl DeviceHook for ID3D12DeviceHook {
        vtable ID3D12DeviceVtable;
        #[release]
        unsafe extern "system" fn device_release(this: *mut c_void) -> u32 {
            context struct {
                tracking: Arc<snowglass_producer_d3d12::Tracking>,
            }
        }

        #[vtable = ID3D12Device_Vtbl]
        #[base = CreateCommandList]
        #[fallback = E_FAIL]
        #[postfix(track_created_command_list(interface_id, output, result))]
        unsafe extern "system" fn create_command_list(this: *mut c_void, node_mask: u32, list_type: D3D12_COMMAND_LIST_TYPE, allocator: *mut c_void, initial_state: *mut c_void, interface_id: *const GUID, output: *mut *mut c_void) -> HRESULT {
            context |state| {
                tracking: Arc<snowglass_producer_d3d12::Tracking> = state.tracking().clone(),
            }
        }

        #[vtable = ID3D12Device4_Vtbl]
        #[base = CreateCommandList1]
        #[fallback = E_FAIL]
        #[postfix(track_created_command_list1(interface_id, output, result))]
        unsafe extern "system" fn create_command_list1(this: *mut c_void, node_mask: u32, list_type: D3D12_COMMAND_LIST_TYPE, flags: D3D12_COMMAND_LIST_FLAGS, interface_id: *const GUID, output: *mut *mut c_void) -> HRESULT {
            context |state| {
                tracking: Arc<snowglass_producer_d3d12::Tracking> = state.tracking().clone(),
            }
        }

        #[vtable = ID3D12Device_Vtbl]
        #[base = CreateDescriptorHeap]
        #[fallback = E_FAIL]
        #[postfix(track_descriptor_heap(desc, output, result))]
        unsafe extern "system" fn create_descriptor_heap(this: *mut c_void, desc: *const D3D12_DESCRIPTOR_HEAP_DESC, interface_id: *const GUID, output: *mut *mut c_void) -> HRESULT {
            context |state| {
                tracking: Arc<snowglass_producer_d3d12::Tracking> = state.tracking().clone(),
            }
        }

        #[vtable = ID3D12Device_Vtbl]
        #[base = CreateShaderResourceView]
        #[postfix(record_shader_resource_view(resource, desc, destination))]
        unsafe extern "system" fn create_shader_resource_view(this: *mut c_void, resource: *mut c_void, desc: *const D3D12_SHADER_RESOURCE_VIEW_DESC, destination: D3D12_CPU_DESCRIPTOR_HANDLE) {
            context |state| {
                tracking: Arc<snowglass_producer_d3d12::Tracking> = state.tracking().clone(),
            }
        }

        #[vtable = ID3D12Device_Vtbl]
        #[base = CreateRenderTargetView]
        #[postfix(record_render_target_view(resource, desc, destination))]
        unsafe extern "system" fn create_render_target_view(this: *mut c_void, resource: *mut c_void, desc: *const D3D12_RENDER_TARGET_VIEW_DESC, destination: D3D12_CPU_DESCRIPTOR_HANDLE) {
            context |state| {
                tracking: Arc<snowglass_producer_d3d12::Tracking> = state.tracking().clone(),
            }
        }

        #[vtable = ID3D12Device_Vtbl]
        #[base = CopyDescriptorsSimple]
        #[postfix(record_copy_descriptors_simple(count, destination, source, heap_type))]
        unsafe extern "system" fn copy_descriptors_simple(this: *mut c_void, count: u32, destination: D3D12_CPU_DESCRIPTOR_HANDLE, source: D3D12_CPU_DESCRIPTOR_HANDLE, heap_type: D3D12_DESCRIPTOR_HEAP_TYPE) {
            context |state| {
                tracking: Arc<snowglass_producer_d3d12::Tracking> = state.tracking().clone(),
            }
        }
    }
}

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
    let already_hooked = DEVICES
        .get(&(base as usize))
        .is_some_and(|hook| unsafe { *base.cast::<*const usize>() == hook.patched_vtable() });

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
    snowglass_hook::widest_interfaces! {
        from device => InstallFn;
        install install_device_kind;
        ID3D12Device14 => ID3D12Device14_Vtbl,
        ID3D12Device13 => ID3D12Device13_Vtbl,
        ID3D12Device12 => ID3D12Device12_Vtbl,
        ID3D12Device11 => ID3D12Device11_Vtbl,
        ID3D12Device10 => ID3D12Device10_Vtbl,
        ID3D12Device9 => ID3D12Device9_Vtbl,
        ID3D12Device8 => ID3D12Device8_Vtbl,
        ID3D12Device7 => ID3D12Device7_Vtbl,
        ID3D12Device6 => ID3D12Device6_Vtbl,
        ID3D12Device5 => ID3D12Device5_Vtbl,
        ID3D12Device4 => ID3D12Device4_Vtbl,
        ID3D12Device3 => ID3D12Device3_Vtbl,
        ID3D12Device2 => ID3D12Device2_Vtbl,
        ID3D12Device1 => ID3D12Device1_Vtbl,
        ID3D12Device => ID3D12Device_Vtbl,
    }
}

unsafe fn install_device_kind<A: Vtable>(
    device: *mut c_void,
    tracking: Arc<snowglass_producer_d3d12::Tracking>,
) -> bool {
    if device.is_null() {
        return false;
    }
    let current = unsafe { *device.cast::<*const usize>() };
    let original = match DEVICES.get(&(device as usize)) {
        Some(existing) => {
            if current == existing.patched_vtable() {
                if A::supported_by(existing.vtable_type()) {
                    return true;
                }
                existing.original_vtable()
            } else {
                current
            }
        }
        None => current,
    };
    unsafe { ID3D12DeviceHook::<A>::install_from(device, original, tracking) }
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
    let interfaces = snowglass_hook::widest_interfaces! {
        from &base => InstallFn;
        install install_command_list_kind;
        ID3D12GraphicsCommandList10 => ID3D12GraphicsCommandList10_Vtbl,
        ID3D12GraphicsCommandList9 => ID3D12GraphicsCommandList9_Vtbl,
        ID3D12GraphicsCommandList8 => ID3D12GraphicsCommandList8_Vtbl,
        ID3D12GraphicsCommandList7 => ID3D12GraphicsCommandList7_Vtbl,
        ID3D12GraphicsCommandList6 => ID3D12GraphicsCommandList6_Vtbl,
        ID3D12GraphicsCommandList5 => ID3D12GraphicsCommandList5_Vtbl,
        ID3D12GraphicsCommandList4 => ID3D12GraphicsCommandList4_Vtbl,
        ID3D12GraphicsCommandList3 => ID3D12GraphicsCommandList3_Vtbl,
        ID3D12GraphicsCommandList2 => ID3D12GraphicsCommandList2_Vtbl,
        ID3D12GraphicsCommandList1 => ID3D12GraphicsCommandList1_Vtbl,
        ID3D12GraphicsCommandList => ID3D12GraphicsCommandList_Vtbl,
    };
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
    let current = unsafe { *list.cast::<*const usize>() };
    let original = match COMMAND_LISTS.get(&(list as usize)) {
        Some(existing) => {
            if current == existing.patched_vtable() {
                if A::supported_by(existing.vtable_type()) {
                    return true;
                }
                existing.original_vtable()
            } else {
                current
            }
        }
        None => current,
    };
    unsafe { ID3D12GraphicsCommandListHook::<A>::install_from(list, original, tracking) }
}

fn device_method<T>(device: *mut c_void, select: impl FnOnce(&dyn DeviceHook) -> T) -> Option<T> {
    DEVICES.get(&(device as usize)).map(|hook| select(&**hook))
}

fn device_tracking(device: *mut c_void) -> Option<Arc<snowglass_producer_d3d12::Tracking>> {
    device_method(device, |hook| hook.tracking().clone())
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

unsafe fn track_created_command_list(
    interface_id: *const GUID,
    output: *mut *mut c_void,
    result: HRESULT,
    context: &CreateCommandListContext<'_>,
) {
    if result.is_ok() && !output.is_null() {
        unsafe { install_command_list(*output, interface_id, context.tracking.clone()) };
    }
}

unsafe fn track_created_command_list1(
    interface_id: *const GUID,
    output: *mut *mut c_void,
    result: HRESULT,
    context: &CreateCommandList1Context<'_>,
) {
    if result.is_ok() && !output.is_null() {
        unsafe { install_command_list(*output, interface_id, context.tracking.clone()) };
    }
}

unsafe fn track_descriptor_heap(
    desc: *const D3D12_DESCRIPTOR_HEAP_DESC,
    output: *mut *mut c_void,
    result: HRESULT,
    context: &CreateDescriptorHeapContext<'_>,
) {
    if result.is_ok() {
        unsafe { context.tracking.record_descriptor_heap(desc, output) };
    }
}

unsafe fn record_shader_resource_view(
    resource: *mut c_void,
    desc: *const D3D12_SHADER_RESOURCE_VIEW_DESC,
    destination: D3D12_CPU_DESCRIPTOR_HANDLE,
    context: &CreateShaderResourceViewContext<'_>,
) {
    unsafe { context.tracking.record_shader_resource_view(resource, desc, destination) };
}

unsafe fn record_render_target_view(
    resource: *mut c_void,
    desc: *const D3D12_RENDER_TARGET_VIEW_DESC,
    destination: D3D12_CPU_DESCRIPTOR_HANDLE,
    context: &CreateRenderTargetViewContext<'_>,
) {
    unsafe { context.tracking.record_render_target_view(resource, desc, destination) };
}

unsafe fn record_copy_descriptors_simple(
    count: u32,
    destination: D3D12_CPU_DESCRIPTOR_HANDLE,
    source: D3D12_CPU_DESCRIPTOR_HANDLE,
    heap_type: D3D12_DESCRIPTOR_HEAP_TYPE,
    context: &CopyDescriptorsSimpleContext<'_>,
) {
    context
        .tracking
        .record_copy_descriptors_simple(count, destination, source, heap_type);
}

unsafe fn track_reset(
    this: *mut c_void,
    allocator: *mut c_void,
    initial_state: *mut c_void,
    mut next: ResetContext<'_>,
) -> HRESULT {
    let tracking = next.tracking.clone();
    let patched_vtable = next.patched_vtable;
    let result = snowglass_hook::next!(next, this, allocator, initial_state);
    // d3d12.dll re-initializes the recycled command list inside Reset and
    // rewrites the object's vtable pointer, discarding the per-object clone.
    // Reinstall the clone after forwarding so tracking survives the reset.
    unsafe { *this.cast::<*const usize>() = *patched_vtable };
    if result.is_ok() {
        tracking.command_reset(this);
    }
    result
}

macro_rules! command_list_post_hook {
    ($hook:ident, $context:ident($($argument:ident: $argument_type:ty),* $(,)?), |$tracking:ident, $this:ident| $after:block) => {
        unsafe fn $hook(
            $this: *mut c_void,
            $($argument: $argument_type,)*
            context: &$context<'_>,
        ) {
            let _ = ($($argument,)*);
            let $tracking = context.tracking;
            $after
        }
    };
}

command_list_post_hook!(record_viewports, SetViewportsContext(count: u32, viewports: *const D3D12_VIEWPORT), |tracking, this| {
    unsafe { tracking.record_viewports(this, count, viewports) };
});
command_list_post_hook!(record_scissors, SetScissorsContext(count: u32, scissors: *const RECT), |tracking, this| {
    unsafe { tracking.record_scissors(this, count, scissors) };
});
command_list_post_hook!(record_draw_instanced, DrawInstancedContext(vertex_count: u32, instance_count: u32, start_vertex: u32, start_instance: u32), |tracking, this| {
    tracking.record_draw(this);
});
command_list_post_hook!(record_draw_indexed_instanced, DrawIndexedInstancedContext(index_count: u32, instance_count: u32, start_index: u32, base_vertex: i32, start_instance: u32), |tracking, this| {
    tracking.record_draw(this);
});
command_list_post_hook!(record_resource_barrier, ResourceBarrierContext(count: u32, barriers: *const D3D12_RESOURCE_BARRIER), |tracking, this| {
    unsafe { tracking.record_resource_barrier(this, count, barriers) };
});
command_list_post_hook!(record_root_descriptor_table, SetGraphicsRootDescriptorTableContext(root_index: u32, descriptor: D3D12_GPU_DESCRIPTOR_HANDLE), |tracking, this| {
    tracking.record_root_descriptor_table(this, root_index, descriptor);
});
command_list_post_hook!(record_root_signature, SetGraphicsRootSignatureContext(signature: *mut c_void), |tracking, this| {
    tracking.record_root_signature(this);
});
command_list_post_hook!(record_render_targets, OmSetRenderTargetsContext(count: u32, descriptors: *const D3D12_CPU_DESCRIPTOR_HANDLE, contiguous: BOOL, depth: *const D3D12_CPU_DESCRIPTOR_HANDLE), |tracking, this| {
    unsafe { tracking.record_render_targets(this, count, descriptors) };
});
command_list_post_hook!(record_enhanced_barrier, EnhancedBarrierContext(count: u32, groups: *const D3D12_BARRIER_GROUP), |tracking, this| {
    unsafe { tracking.record_enhanced_barrier(this, count, groups) };
});
