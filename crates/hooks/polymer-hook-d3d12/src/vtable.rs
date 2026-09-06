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
            ID3D12Device1, ID3D12Device2, ID3D12Device3, ID3D12Device4, ID3D12Device5,
            ID3D12Device6, ID3D12Device7, ID3D12Device8, ID3D12Device9, ID3D12Device10,
            ID3D12Device11, ID3D12Device12, ID3D12Device13, ID3D12Device14,
            ID3D12GraphicsCommandList, ID3D12GraphicsCommandList1, ID3D12GraphicsCommandList2,
            ID3D12GraphicsCommandList3, ID3D12GraphicsCommandList4, ID3D12GraphicsCommandList5,
            ID3D12GraphicsCommandList6, ID3D12GraphicsCommandList7, ID3D12GraphicsCommandList8,
            ID3D12GraphicsCommandList9, ID3D12GraphicsCommandList10,
        },
    },
    core::{BOOL, GUID, HRESULT, IUnknown, Interface},
};

polymer_hook::vtable! {
    [ID3D12CommandQueue],
    [
        ID3D12GraphicsCommandList,
        ID3D12GraphicsCommandList1,
        ID3D12GraphicsCommandList2,
        ID3D12GraphicsCommandList3,
        ID3D12GraphicsCommandList4,
        ID3D12GraphicsCommandList5,
        ID3D12GraphicsCommandList6,
        ID3D12GraphicsCommandList7,
        ID3D12GraphicsCommandList8,
        ID3D12GraphicsCommandList9,
        ID3D12GraphicsCommandList10,
    ],
    [
        ID3D12Device,
        ID3D12Device1,
        ID3D12Device2,
        ID3D12Device3,
        ID3D12Device4,
        ID3D12Device5,
        ID3D12Device6,
        ID3D12Device7,
        ID3D12Device8,
        ID3D12Device9,
        ID3D12Device10,
        ID3D12Device11,
        ID3D12Device12,
        ID3D12Device13,
        ID3D12Device14,
    ],
}

polymer_hook::com_vtable! {
    /// Overlays every graphics command-list interface generation on one
    /// allocation. Narrower interfaces are prefixes of this storage.
    pub(crate) union ID3D12GraphicsCommandListVtable for ID3D12GraphicsCommandList10;
}

polymer_hook::com_vtable! {
    /// Overlays every device interface generation on one allocation, mirroring
    /// [`ID3D12GraphicsCommandListVtable`].
    pub(crate) union ID3D12DeviceVtable for ID3D12Device14;
}

polymer_hook::com_vtable! {
    union ID3D12CommandQueueVtable for ID3D12CommandQueue;
}

polymer_hook::impl_com_hook! {
    trait QueueHook;
    impl QueueHook for ID3D12CommandQueueHook {
        #![vtable = ID3D12CommandQueueVtable]
        #[interfaces] { ID3D12CommandQueue }
        #[state = DeviceContext]
        #[release]
        unsafe extern "system" fn queue_release(this: *mut c_void) -> u32;
        #[base = ID3D12CommandQueue::ExecuteCommandLists]
        #[infix(track_submission)]
        unsafe extern "system" fn execute_command_lists(this: *mut c_void, count: u32, lists: *const *mut c_void);
    }
}

pub(crate) struct CommandListContext {
    lifetime: Arc<CommandListLifetime>,
    pub(crate) tracking: Arc<polymer_producer_d3d12::Tracking>,
}

struct CommandListLifetime {
    tracking: Arc<polymer_producer_d3d12::Tracking>,
    id: usize,
}
impl Drop for CommandListLifetime {
    fn drop(&mut self) {
        self.tracking.command_reset(self.id as *mut c_void);
    }
}

pub(crate) struct DeviceContext {
    pub(crate) tracking: Arc<polymer_producer_d3d12::Tracking>,
}

polymer_hook::impl_com_hook! {
    pub(crate) trait CommandListHook;
    impl CommandListHook for ID3D12GraphicsCommandListHook {
        #![vtable = ID3D12GraphicsCommandListVtable]
        #[interfaces] {
            ID3D12GraphicsCommandList10,
            ID3D12GraphicsCommandList9,
            ID3D12GraphicsCommandList8,
            ID3D12GraphicsCommandList7,
            ID3D12GraphicsCommandList6,
            ID3D12GraphicsCommandList5,
            ID3D12GraphicsCommandList4,
            ID3D12GraphicsCommandList3,
            ID3D12GraphicsCommandList2,
            ID3D12GraphicsCommandList1,
            ID3D12GraphicsCommandList,
        }
        #[state = CommandListContext]

        #[release]
        unsafe extern "system" fn command_list_release(this: *mut c_void) -> u32;

        #[base = ID3D12GraphicsCommandList::Reset]
        #[fallback = E_FAIL]
        #[infix(track_reset)]
        unsafe extern "system" fn reset(this: *mut c_void, allocator: *mut c_void, initial_state: *mut c_void) -> HRESULT {
            #[context] |state| {
                patched_vtable: *const c_void = state.patched_vtable(),
            }
        }

        #[base = ID3D12GraphicsCommandList::ExecuteBundle]
        #[postfix(|_result, context| context.state.tracking.execute_bundle(this, bundle))]
        unsafe extern "system" fn execute_bundle(this: *mut c_void, bundle: *mut c_void);

        #[base = ID3D12GraphicsCommandList::CopyTextureRegion]
        #[postfix(|_result, context| unsafe { context.state.tracking.copy_texture(this, destination, x, y, source, bounds) })]
        unsafe extern "system" fn copy_texture_region(this: *mut c_void, destination: *const windows::Win32::Graphics::Direct3D12::D3D12_TEXTURE_COPY_LOCATION,
            x: u32, y: u32, z: u32, source: *const windows::Win32::Graphics::Direct3D12::D3D12_TEXTURE_COPY_LOCATION,
            bounds: *const windows::Win32::Graphics::Direct3D12::D3D12_BOX);

        #[base = ID3D12GraphicsCommandList::ResolveSubresource]
        #[postfix(|_result, context| unsafe {
            context.state.tracking.resolve_subresource(this, destination, destination_subresource, source, source_subresource);
        })]
        unsafe extern "system" fn resolve_subresource(this: *mut c_void, destination: *mut c_void, destination_subresource: u32,
            source: *mut c_void, source_subresource: u32, format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT);

        #[base = ID3D12GraphicsCommandList::CopyResource]
        #[postfix(|_result, context| unsafe { context.state.tracking.copy_resource(this, destination, source) })]
        unsafe extern "system" fn copy_resource(this: *mut c_void, destination: *mut c_void, source: *mut c_void);

        #[base = ID3D12GraphicsCommandList::RSSetViewports]
        #[postfix(|_result, context| unsafe {
            context.state.tracking.record_viewports(this, count, viewports)
        })]
        unsafe extern "system" fn set_viewports(this: *mut c_void, count: u32, viewports: *const D3D12_VIEWPORT) ;

        #[base = ID3D12GraphicsCommandList::RSSetScissorRects]
        #[postfix(|_result, context| unsafe {
            context.state.tracking.record_scissors(this, count, scissors)
        })]
        unsafe extern "system" fn set_scissors(this: *mut c_void, count: u32, scissors: *const RECT) ;

        #[base = ID3D12GraphicsCommandList::DrawInstanced]
        #[postfix(|_result, context| context.state.tracking.record_draw(this))]
        unsafe extern "system" fn draw_instanced(this: *mut c_void, vertex_count: u32, instance_count: u32, start_vertex: u32, start_instance: u32) ;

        #[base = ID3D12GraphicsCommandList::DrawIndexedInstanced]
        #[postfix(|_result, context| context.state.tracking.record_draw(this))]
        unsafe extern "system" fn draw_indexed_instanced(this: *mut c_void, index_count: u32, instance_count: u32, start_index: u32, base_vertex: i32, start_instance: u32) ;

        #[base = ID3D12GraphicsCommandList::ResourceBarrier]
        #[postfix(|_result, context| unsafe {
            context.state.tracking.record_resource_barrier(this, count, barriers)
        })]
        unsafe extern "system" fn resource_barrier(this: *mut c_void, count: u32, barriers: *const D3D12_RESOURCE_BARRIER) ;

        #[base = ID3D12GraphicsCommandList::SetGraphicsRootDescriptorTable]
        #[postfix(|_result, context| {
            context.state.tracking.record_root_descriptor_table(this, root_index, descriptor)
        })]
        unsafe extern "system" fn set_graphics_root_descriptor_table(this: *mut c_void, root_index: u32, descriptor: D3D12_GPU_DESCRIPTOR_HANDLE) ;

        #[base = ID3D12GraphicsCommandList::SetDescriptorHeaps]
        #[postfix(|_result, context| unsafe { context.state.tracking.record_descriptor_heaps(this, count, heaps) })]
        unsafe extern "system" fn set_descriptor_heaps(this: *mut c_void, count: u32, heaps: *const *mut c_void);

        #[base = ID3D12GraphicsCommandList::SetGraphicsRootSignature]
        #[postfix(|_result, context| context.state.tracking.record_root_signature(this, signature))]
        unsafe extern "system" fn set_graphics_root_signature(this: *mut c_void, signature: *mut c_void) ;

        #[base = ID3D12GraphicsCommandList::OMSetRenderTargets]
        #[postfix(|_result, context| unsafe {
            context.state.tracking.record_render_targets(this, count, descriptors, contiguous.as_bool())
        })]
        unsafe extern "system" fn om_set_render_targets(this: *mut c_void, count: u32, descriptors: *const D3D12_CPU_DESCRIPTOR_HANDLE, contiguous: BOOL, depth: *const D3D12_CPU_DESCRIPTOR_HANDLE) ;

        #[base = ID3D12GraphicsCommandList::ClearRenderTargetView]
        #[postfix(|_result, context| unsafe { context.state.tracking.record_clear(this, descriptor, count, rects) })]
        unsafe extern "system" fn clear_render_target(this: *mut c_void, descriptor: D3D12_CPU_DESCRIPTOR_HANDLE, color: *const f32, count: u32, rects: *const RECT);

        #[base = ID3D12GraphicsCommandList7::Barrier]
        #[postfix(|_result, context| unsafe {
            context.state.tracking.record_enhanced_barrier(this, count, groups)
        })]
        unsafe extern "system" fn enhanced_barrier(this: *mut c_void, count: u32, groups: *const D3D12_BARRIER_GROUP) ;
    }
}

polymer_hook::impl_com_hook! {
    pub(crate) trait DeviceHook;
    impl DeviceHook for ID3D12DeviceHook {
        #![vtable = ID3D12DeviceVtable]
        #[interfaces] {
            ID3D12Device14,
            ID3D12Device13,
            ID3D12Device12,
            ID3D12Device11,
            ID3D12Device10,
            ID3D12Device9,
            ID3D12Device8,
            ID3D12Device7,
            ID3D12Device6,
            ID3D12Device5,
            ID3D12Device4,
            ID3D12Device3,
            ID3D12Device2,
            ID3D12Device1,
            ID3D12Device,
        }
        #[state = DeviceContext]
        #[release]
        unsafe extern "system" fn device_release(this: *mut c_void) -> u32;

        #[base = ID3D12Device::CreateRootSignature]
        #[fallback = E_FAIL]
        #[postfix(|result, context| unsafe { if result.is_ok() { context.state.tracking.root_signature_created(blob, length, output); } })]
        unsafe extern "system" fn create_root_signature(this: *mut c_void, node_mask: u32, blob: *const c_void, length: usize, iid: *const GUID, output: *mut *mut c_void) -> HRESULT;

        #[base = ID3D12Device::CopyDescriptors]
        #[postfix(|_result, context| unsafe { context.state.tracking.copy_descriptors(destination_count, destinations, destination_sizes, source_count, sources, source_sizes, kind) })]
        unsafe extern "system" fn copy_descriptors(this: *mut c_void, destination_count: u32, destinations: *const D3D12_CPU_DESCRIPTOR_HANDLE, destination_sizes: *const u32,
            source_count: u32, sources: *const D3D12_CPU_DESCRIPTOR_HANDLE, source_sizes: *const u32, kind: D3D12_DESCRIPTOR_HEAP_TYPE);

        #[base = ID3D12Device::CreateCommandQueue]
        #[fallback = E_FAIL]
        #[postfix(|result, context| unsafe {
            if result.is_ok() && !output.is_null() && !(*output).is_null() {
                ID3D12CommandQueueHook::install_all_interfaces(*output, DeviceContext { tracking: context.state.tracking.clone() });
            }
        })]
        unsafe extern "system" fn create_command_queue(this: *mut c_void, desc: *const windows::Win32::Graphics::Direct3D12::D3D12_COMMAND_QUEUE_DESC, interface_id: *const GUID, output: *mut *mut c_void) -> HRESULT;

        #[base = ID3D12Device::CreateCommandList]
        #[fallback = E_FAIL]
        #[postfix(track_created_command_list(interface_id, output, result))]
        unsafe extern "system" fn create_command_list(this: *mut c_void, node_mask: u32, list_type: D3D12_COMMAND_LIST_TYPE, allocator: *mut c_void, initial_state: *mut c_void, interface_id: *const GUID, output: *mut *mut c_void) -> HRESULT ;

        #[base = ID3D12Device4::CreateCommandList1]
        #[fallback = E_FAIL]
        #[postfix(track_created_command_list1(interface_id, output, result))]
        unsafe extern "system" fn create_command_list1(this: *mut c_void, node_mask: u32, list_type: D3D12_COMMAND_LIST_TYPE, flags: D3D12_COMMAND_LIST_FLAGS, interface_id: *const GUID, output: *mut *mut c_void) -> HRESULT ;

        #[base = ID3D12Device::CreateDescriptorHeap]
        #[fallback = E_FAIL]
        #[postfix(track_descriptor_heap(desc, output, result))]
        unsafe extern "system" fn create_descriptor_heap(this: *mut c_void, desc: *const D3D12_DESCRIPTOR_HEAP_DESC, interface_id: *const GUID, output: *mut *mut c_void) -> HRESULT ;

        #[base = ID3D12Device::CreateShaderResourceView]
        #[postfix(|_result, context| unsafe {
            context.state.tracking.record_shader_resource_view(resource, desc, destination)
        })]
        unsafe extern "system" fn create_shader_resource_view(this: *mut c_void, resource: *mut c_void, desc: *const D3D12_SHADER_RESOURCE_VIEW_DESC, destination: D3D12_CPU_DESCRIPTOR_HANDLE) ;

        #[base = ID3D12Device::CreateRenderTargetView]
        #[postfix(|_result, context| unsafe {
            context.state.tracking.record_render_target_view(resource, desc, destination)
        })]
        unsafe extern "system" fn create_render_target_view(this: *mut c_void, resource: *mut c_void, desc: *const D3D12_RENDER_TARGET_VIEW_DESC, destination: D3D12_CPU_DESCRIPTOR_HANDLE) ;

        #[base = ID3D12Device::CopyDescriptorsSimple]
        #[postfix(|_result, context| {
            context.state.tracking.record_copy_descriptors_simple(
                count,
                destination,
                source,
                heap_type,
            )
        })]
        unsafe extern "system" fn copy_descriptors_simple(this: *mut c_void, count: u32, destination: D3D12_CPU_DESCRIPTOR_HANDLE, source: D3D12_CPU_DESCRIPTOR_HANDLE, heap_type: D3D12_DESCRIPTOR_HEAP_TYPE) ;
    }
}

impl Clone for CommandListContext {
    fn clone(&self) -> Self {
        Self {
            lifetime: self.lifetime.clone(),
            tracking: self.tracking.clone(),
        }
    }
}

impl Clone for DeviceContext {
    fn clone(&self) -> Self {
        Self {
            tracking: self.tracking.clone(),
        }
    }
}

/// Installs render-target tracking on a device returned by
/// `D3D12CreateDevice`. Command lists are hooked as the device creates them.
///
/// # Safety
///
/// `output` must be null or point to a COM interface pointer for a live
/// `ID3D12Device`.
pub(crate) unsafe fn install_device(
    output: *mut *mut c_void,
    tracking: Arc<polymer_producer_d3d12::Tracking>,
) {
    if !polymer_hook::config::tracking_enabled() || output.is_null() {
        return;
    }
    match unsafe { try_install_device(output, tracking) } {
        Ok(true) => tracing::debug!("polymer: installed D3D12 viewport/scissor tracking"),
        Ok(false) => {}
        Err(error) => {
            tracing::debug!("polymer: failed to install D3D12 viewport/scissor tracking: {error}")
        }
    }
}

unsafe fn try_install_device(
    output: *mut *mut c_void,
    tracking: Arc<polymer_producer_d3d12::Tracking>,
) -> Result<bool, String> {
    let raw = unsafe { *output };
    let unknown = unsafe { IUnknown::from_raw_borrowed(&raw) }
        .ok_or_else(|| "D3D12CreateDevice returned a null device".to_owned())?;
    let device: ID3D12Device = unknown.cast().map_err(|error| error.to_string())?;
    if let Some(enhanced_barriers) = tracking.detect_device_features(&device)? {
        tracing::debug!(message = %(if enhanced_barriers {
            "polymer: D3D12 enhanced capture barriers enabled"
        } else {
            "polymer: D3D12 capture using legacy resource barriers"
        }));
    }
    let base = device.as_raw();
    let already_hooked = unsafe { ID3D12DeviceHook::is_installed(base) };

    let installed =
        unsafe { ID3D12DeviceHook::install_all_interfaces(base, DeviceContext { tracking }) };
    if !installed {
        return Err("could not clone the device vtable".to_owned());
    }
    Ok(!already_hooked)
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
    tracking: Arc<polymer_producer_d3d12::Tracking>,
) {
    if list.is_null() || interface_id.is_null() {
        return;
    }
    // Clone the widest interface generation the object implements rather than
    // the generation it was created as: a caller can obtain a wider interface
    // later through QueryInterface (typically the same pointer), and a clone
    // sized to the creation interface would dispatch those wider slots into
    // the uninitialized tail of the clone.
    unsafe {
        ID3D12GraphicsCommandListHook::install_all_interfaces(
            list,
            CommandListContext {
                lifetime: Arc::new(CommandListLifetime {
                    tracking: tracking.clone(),
                    id: list as usize,
                }),
                tracking,
            },
        )
    };
}

fn device_tracking(device: *mut c_void) -> Option<Arc<polymer_producer_d3d12::Tracking>> {
    ID3D12DeviceHook::with_state(device, |state| state.tracking.clone())
}

/// Resolves the tracking context associated with a D3D12 command queue's device.
///
/// # Safety
///
/// `queue` must be null or point to a live `ID3D12CommandQueue`.
pub(crate) unsafe fn queue_tracking(
    queue: *mut c_void,
) -> Option<Arc<polymer_producer_d3d12::Tracking>> {
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
        unsafe { install_command_list(*output, interface_id, context.state.tracking.clone()) };
    }
}

unsafe fn track_created_command_list1(
    interface_id: *const GUID,
    output: *mut *mut c_void,
    result: HRESULT,
    context: &CreateCommandList1Context<'_>,
) {
    if result.is_ok() && !output.is_null() {
        unsafe { install_command_list(*output, interface_id, context.state.tracking.clone()) };
    }
}

unsafe fn track_descriptor_heap(
    desc: *const D3D12_DESCRIPTOR_HEAP_DESC,
    output: *mut *mut c_void,
    result: HRESULT,
    context: &CreateDescriptorHeapContext<'_>,
) {
    if result.is_ok() {
        unsafe { context.state.tracking.record_descriptor_heap(desc, output) };
    }
}

unsafe fn track_submission(
    this: *mut c_void,
    count: u32,
    lists: *const *mut c_void,
    mut next: ExecuteCommandListsContext<'_>,
) {
    let tracking = next.state.tracking.clone();
    unsafe {
        tracking.execute(this as usize, count, lists, || {
            polymer_hook::next!(next, this, count, lists)
        })
    };
}

unsafe fn track_reset(
    this: *mut c_void,
    allocator: *mut c_void,
    initial_state: *mut c_void,
    mut next: ResetContext<'_>,
) -> HRESULT {
    let tracking = next.state.tracking.clone();
    let patched_vtable = next.patched_vtable;
    let result = polymer_hook::next!(next, this, allocator, initial_state);
    // d3d12.dll re-initializes the recycled command list inside Reset and
    // rewrites the object's vtable pointer, discarding the per-object clone.
    // Reinstall the clone after forwarding so tracking survives the reset.
    unsafe { *this.cast::<*const c_void>() = *patched_vtable };
    if result.is_ok() {
        tracking.command_reset(this);
    }
    result
}
