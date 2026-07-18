//! Capture-region tracking derived from D3D12 command-list state.

use dashmap::DashMap;
use retour::GenericDetour;
use snowglass_producer_d3d12::{D3d12CaptureRegion, D3d12CaptureSourceState};
use std::{
    collections::{HashMap, HashSet},
    ffi::c_void,
    sync::LazyLock,
};
use windows::{
    Win32::{
        Foundation::RECT,
        Graphics::{
            Direct3D12::{
                D3D12_BARRIER_GROUP, D3D12_BARRIER_LAYOUT_COMMON, D3D12_BARRIER_TYPE_TEXTURE,
                D3D12_COMMAND_LIST_TYPE_DIRECT, D3D12_CPU_DESCRIPTOR_HANDLE,
                D3D12_DESCRIPTOR_HEAP_DESC, D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                D3D12_DESCRIPTOR_HEAP_TYPE, D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                D3D12_FEATURE_D3D12_OPTIONS12, D3D12_FEATURE_DATA_D3D12_OPTIONS12,
                D3D12_GPU_DESCRIPTOR_HANDLE, D3D12_RENDER_TARGET_VIEW_DESC, D3D12_RESOURCE_BARRIER,
                D3D12_RESOURCE_BARRIER_TYPE_TRANSITION, D3D12_RESOURCE_STATE_PRESENT,
                D3D12_SHADER_RESOURCE_VIEW_DESC, D3D12_VIEWPORT, ID3D12CommandAllocator,
                ID3D12DescriptorHeap, ID3D12Device, ID3D12GraphicsCommandList,
                ID3D12GraphicsCommandList7, ID3D12PipelineState, ID3D12Resource,
            },
            Dxgi::IDXGISwapChain3,
        },
    },
    core::{BOOL, GUID, HRESULT, IUnknown, Interface},
};

type ResetFn = unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> HRESULT;
type SetViewportsFn = unsafe extern "system" fn(*mut c_void, u32, *const D3D12_VIEWPORT);
type SetScissorsFn = unsafe extern "system" fn(*mut c_void, u32, *const RECT);
type DrawInstancedFn = unsafe extern "system" fn(*mut c_void, u32, u32, u32, u32);
type DrawIndexedInstancedFn = unsafe extern "system" fn(*mut c_void, u32, u32, u32, i32, u32);
type ResourceBarrierFn = unsafe extern "system" fn(*mut c_void, u32, *const D3D12_RESOURCE_BARRIER);
type CreateDescriptorHeapFn = unsafe extern "system" fn(
    *mut c_void,
    *const D3D12_DESCRIPTOR_HEAP_DESC,
    *const GUID,
    *mut *mut c_void,
) -> HRESULT;
type CreateShaderResourceViewFn = unsafe extern "system" fn(
    *mut c_void,
    *mut c_void,
    *const D3D12_SHADER_RESOURCE_VIEW_DESC,
    D3D12_CPU_DESCRIPTOR_HANDLE,
);
type CreateRenderTargetViewFn = unsafe extern "system" fn(
    *mut c_void,
    *mut c_void,
    *const D3D12_RENDER_TARGET_VIEW_DESC,
    D3D12_CPU_DESCRIPTOR_HANDLE,
);
type CopyDescriptorsSimpleFn = unsafe extern "system" fn(
    *mut c_void,
    u32,
    D3D12_CPU_DESCRIPTOR_HANDLE,
    D3D12_CPU_DESCRIPTOR_HANDLE,
    D3D12_DESCRIPTOR_HEAP_TYPE,
);
type SetGraphicsRootDescriptorTableFn =
    unsafe extern "system" fn(*mut c_void, u32, D3D12_GPU_DESCRIPTOR_HANDLE);
type SetGraphicsRootSignatureFn = unsafe extern "system" fn(*mut c_void, *mut c_void);
type OmSetRenderTargetsFn = unsafe extern "system" fn(
    *mut c_void,
    u32,
    *const D3D12_CPU_DESCRIPTOR_HANDLE,
    BOOL,
    *const D3D12_CPU_DESCRIPTOR_HANDLE,
);
type EnhancedBarrierFn = unsafe extern "system" fn(*mut c_void, u32, *const D3D12_BARRIER_GROUP);

struct Detours {
    reset: GenericDetour<ResetFn>,
    set_viewports: GenericDetour<SetViewportsFn>,
    set_scissors: GenericDetour<SetScissorsFn>,
    draw_instanced: GenericDetour<DrawInstancedFn>,
    draw_indexed_instanced: GenericDetour<DrawIndexedInstancedFn>,
    resource_barrier: GenericDetour<ResourceBarrierFn>,
    create_descriptor_heap: GenericDetour<CreateDescriptorHeapFn>,
    create_shader_resource_view: GenericDetour<CreateShaderResourceViewFn>,
    create_render_target_view: GenericDetour<CreateRenderTargetViewFn>,
    copy_descriptors_simple: GenericDetour<CopyDescriptorsSimpleFn>,
    set_graphics_root_descriptor_table: GenericDetour<SetGraphicsRootDescriptorTableFn>,
    set_graphics_root_signature: GenericDetour<SetGraphicsRootSignatureFn>,
    om_set_render_targets: GenericDetour<OmSetRenderTargetsFn>,
}

#[derive(Clone)]
struct Candidate {
    region: D3d12CaptureRegion,
    draw_area: u64,
    target_extent: (u32, u32),
    source: Option<TrackedResource>,
}

impl Candidate {
    fn rank(&self) -> (u64, u64) {
        let source_pixels = self.source.as_ref().map_or(0, |source| {
            u64::from(source.width) * u64::from(source.height)
        });
        (self.draw_area, source_pixels)
    }
}

pub(crate) struct Presentation {
    pub(crate) region: Option<D3d12CaptureRegion>,
    pub(crate) source: Option<TrackedResource>,
}

#[derive(Clone)]
pub(crate) struct TrackedResource {
    pub(crate) resource: ID3D12Resource,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) format: u32,
    pub(crate) state: Option<D3d12CaptureSourceState>,
}

#[derive(Clone, Copy)]
struct TrackedTarget {
    key: usize,
    width: u32,
    height: u32,
}

#[derive(Default)]
struct CommandState {
    viewport: Option<D3D12_VIEWPORT>,
    scissor: Option<RECT>,
    candidate: Option<Candidate>,
    render_target: Option<TrackedTarget>,
    root_sources: HashMap<u32, TrackedResource>,
    recorded_targets: HashSet<usize>,
}

struct HeapInfo {
    cpu_base: usize,
    gpu_base: u64,
    count: u32,
    increment: u32,
}

static DETOURS: std::sync::OnceLock<Detours> = std::sync::OnceLock::new();
static ENHANCED_BARRIER: std::sync::OnceLock<GenericDetour<EnhancedBarrierFn>> =
    std::sync::OnceLock::new();
static COMMANDS: LazyLock<DashMap<usize, CommandState>> = LazyLock::new(Default::default);
static PRESENT_REGIONS: LazyLock<DashMap<usize, Candidate>> = LazyLock::new(Default::default);
static CPU_DESCRIPTORS: LazyLock<DashMap<usize, TrackedResource>> = LazyLock::new(Default::default);
static RTV_DESCRIPTORS: LazyLock<DashMap<usize, TrackedTarget>> = LazyLock::new(Default::default);
static RESOURCE_STATES: LazyLock<DashMap<usize, D3d12CaptureSourceState>> =
    LazyLock::new(Default::default);
static GPU_HEAPS: LazyLock<DashMap<u64, HeapInfo>> = LazyLock::new(Default::default);
static DESCRIPTOR_INCREMENT: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
static ENHANCED_BARRIERS_SUPPORTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

pub(crate) fn enhanced_barriers_supported() -> bool {
    ENHANCED_BARRIERS_SUPPORTED.get().copied().unwrap_or(false)
}

/// Releases every tracker-owned reference to the swapchain's current buffers.
///
/// DXGI requires all direct and indirect references obtained through
/// `GetBuffer` to be gone before `ResizeBuffers` is called. The tracking maps
/// deliberately retain resources across command-list execution, so their
/// swapchain entries must be invalidated explicitly at this lifecycle boundary.
pub(crate) unsafe fn before_swapchain_resize(swapchain: *mut c_void) {
    let Some(swapchain) = (unsafe { IDXGISwapChain3::from_raw_borrowed(&swapchain) }) else {
        return;
    };
    let Ok(desc) = (unsafe { swapchain.GetDesc() }) else {
        return;
    };
    let buffer_keys = (0..desc.BufferCount)
        .filter_map(|index| unsafe { swapchain.GetBuffer::<ID3D12Resource>(index).ok() })
        .map(|resource| identity_key(&resource))
        .collect::<HashSet<_>>();
    if buffer_keys.is_empty() {
        return;
    }

    CPU_DESCRIPTORS.retain(|_, resource| !buffer_keys.contains(&identity_key(&resource.resource)));
    RTV_DESCRIPTORS.retain(|_, target| !buffer_keys.contains(&target.key));
    PRESENT_REGIONS.retain(|key, candidate| {
        !buffer_keys.contains(key)
            && candidate
                .source
                .as_ref()
                .is_none_or(|source| !buffer_keys.contains(&identity_key(&source.resource)))
    });
    RESOURCE_STATES.retain(|key, _| !buffer_keys.contains(key));
    for mut command in COMMANDS.iter_mut() {
        let state = command.value_mut();
        state
            .root_sources
            .retain(|_, source| !buffer_keys.contains(&identity_key(&source.resource)));
        if state
            .candidate
            .as_ref()
            .and_then(|candidate| candidate.source.as_ref())
            .is_some_and(|source| buffer_keys.contains(&identity_key(&source.resource)))
        {
            state.candidate = None;
        }
        if state
            .render_target
            .is_some_and(|target| buffer_keys.contains(&target.key))
        {
            state.render_target = None;
        }
        state
            .recorded_targets
            .retain(|key| !buffer_keys.contains(key));
    }
}

pub(crate) unsafe fn install_device(output: *mut *mut c_void) {
    if std::env::var_os("SNOWGLASS_TRACK_RENDER_TARGET").is_none()
        || output.is_null()
        || DETOURS.get().is_some()
    {
        return;
    }
    if let Err(error) = unsafe { try_install_device(output) } {
        crate::d3d12_log(format!(
            "snowglass: failed to install D3D12 viewport/scissor tracking: {error}"
        ));
    } else {
        crate::d3d12_log("snowglass: installed D3D12 viewport/scissor tracking");
    }
}

unsafe fn try_install_device(output: *mut *mut c_void) -> Result<(), String> {
    let raw = unsafe { *output };
    let unknown = unsafe { IUnknown::from_raw_borrowed(&raw) }
        .ok_or_else(|| "D3D12CreateDevice returned a null device".to_owned())?;
    let device: ID3D12Device = unknown.cast().map_err(|error| error.to_string())?;
    let (allocator, list): (ID3D12CommandAllocator, ID3D12GraphicsCommandList) = unsafe {
        let allocator = device
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
        (allocator, list)
    };
    let _keep_allocator_alive = allocator;
    let vtable = Interface::vtable(&list);
    let device_vtable = Interface::vtable(&device);
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
    let enhanced_list = enhanced_barriers_advertised
        .then(|| list.cast::<ID3D12GraphicsCommandList7>().ok())
        .flatten();
    let enhanced_barriers_supported = enhanced_list.is_some();
    let _ = ENHANCED_BARRIERS_SUPPORTED.set(enhanced_barriers_supported);
    crate::d3d12_log(if enhanced_barriers_supported {
        "snowglass: D3D12 enhanced capture barriers enabled"
    } else {
        "snowglass: D3D12 capture using legacy resource barriers"
    });
    let _ = DESCRIPTOR_INCREMENT.set(unsafe {
        device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV)
    });
    let detours = unsafe {
        Detours {
            reset: GenericDetour::new(vtable.Reset, reset).map_err(|error| error.to_string())?,
            set_viewports: GenericDetour::new(vtable.RSSetViewports, set_viewports)
                .map_err(|error| error.to_string())?,
            set_scissors: GenericDetour::new(vtable.RSSetScissorRects, set_scissors)
                .map_err(|error| error.to_string())?,
            draw_instanced: GenericDetour::new(vtable.DrawInstanced, draw_instanced)
                .map_err(|error| error.to_string())?,
            draw_indexed_instanced: GenericDetour::new(
                vtable.DrawIndexedInstanced,
                draw_indexed_instanced,
            )
            .map_err(|error| error.to_string())?,
            resource_barrier: GenericDetour::new(vtable.ResourceBarrier, resource_barrier)
                .map_err(|error| error.to_string())?,
            create_descriptor_heap: GenericDetour::new(
                device_vtable.CreateDescriptorHeap,
                create_descriptor_heap,
            )
            .map_err(|error| error.to_string())?,
            create_shader_resource_view: GenericDetour::new(
                device_vtable.CreateShaderResourceView,
                create_shader_resource_view,
            )
            .map_err(|error| error.to_string())?,
            create_render_target_view: GenericDetour::new(
                device_vtable.CreateRenderTargetView,
                create_render_target_view,
            )
            .map_err(|error| error.to_string())?,
            copy_descriptors_simple: GenericDetour::new(
                device_vtable.CopyDescriptorsSimple,
                copy_descriptors_simple,
            )
            .map_err(|error| error.to_string())?,
            set_graphics_root_descriptor_table: GenericDetour::new(
                vtable.SetGraphicsRootDescriptorTable,
                set_graphics_root_descriptor_table,
            )
            .map_err(|error| error.to_string())?,
            set_graphics_root_signature: GenericDetour::new(
                vtable.SetGraphicsRootSignature,
                set_graphics_root_signature,
            )
            .map_err(|error| error.to_string())?,
            om_set_render_targets: GenericDetour::new(
                vtable.OMSetRenderTargets,
                om_set_render_targets,
            )
            .map_err(|error| error.to_string())?,
        }
    };
    if DETOURS.set(detours).is_err() {
        return Ok(());
    }
    if let Some(list7) = enhanced_list {
        let detour = unsafe {
            GenericDetour::new(Interface::vtable(&list7).Barrier, enhanced_barrier)
                .map_err(|error| error.to_string())?
        };
        let _ = ENHANCED_BARRIER.set(detour);
    }
    let detours = DETOURS.get().expect("D3D12 tracking detours were set");
    unsafe {
        detours.reset.enable().map_err(|error| error.to_string())?;
        detours
            .set_viewports
            .enable()
            .map_err(|error| error.to_string())?;
        detours
            .set_scissors
            .enable()
            .map_err(|error| error.to_string())?;
        detours
            .draw_instanced
            .enable()
            .map_err(|error| error.to_string())?;
        detours
            .draw_indexed_instanced
            .enable()
            .map_err(|error| error.to_string())?;
        detours
            .resource_barrier
            .enable()
            .map_err(|error| error.to_string())?;
        detours
            .create_descriptor_heap
            .enable()
            .map_err(|error| error.to_string())?;
        detours
            .create_shader_resource_view
            .enable()
            .map_err(|error| error.to_string())?;
        detours
            .create_render_target_view
            .enable()
            .map_err(|error| error.to_string())?;
        detours
            .copy_descriptors_simple
            .enable()
            .map_err(|error| error.to_string())?;
        detours
            .set_graphics_root_descriptor_table
            .enable()
            .map_err(|error| error.to_string())?;
        detours
            .set_graphics_root_signature
            .enable()
            .map_err(|error| error.to_string())?;
        detours
            .om_set_render_targets
            .enable()
            .map_err(|error| error.to_string())?;
        if let Some(barrier) = ENHANCED_BARRIER.get() {
            barrier.enable().map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

unsafe extern "system" fn reset(
    this: *mut c_void,
    allocator: *mut c_void,
    initial_state: *mut c_void,
) -> HRESULT {
    let result = unsafe {
        DETOURS
            .get()
            .expect("reset detour is installed")
            .reset
            .call(this, allocator, initial_state)
    };
    if result.is_ok() {
        COMMANDS.remove(&(this as usize));
    }
    result
}

unsafe extern "system" fn set_viewports(
    this: *mut c_void,
    count: u32,
    viewports: *const D3D12_VIEWPORT,
) {
    unsafe {
        DETOURS
            .get()
            .expect("viewport detour is installed")
            .set_viewports
            .call(this, count, viewports)
    };
    if count != 0 && !viewports.is_null() {
        COMMANDS.entry(this as usize).or_default().viewport = Some(unsafe { *viewports });
    }
}

unsafe extern "system" fn set_scissors(this: *mut c_void, count: u32, scissors: *const RECT) {
    unsafe {
        DETOURS
            .get()
            .expect("scissor detour is installed")
            .set_scissors
            .call(this, count, scissors)
    };
    if count != 0 && !scissors.is_null() {
        COMMANDS.entry(this as usize).or_default().scissor = Some(unsafe { *scissors });
    }
}

unsafe extern "system" fn create_descriptor_heap(
    this: *mut c_void,
    desc: *const D3D12_DESCRIPTOR_HEAP_DESC,
    iid: *const GUID,
    output: *mut *mut c_void,
) -> HRESULT {
    let result = unsafe {
        DETOURS
            .get()
            .expect("descriptor heap detour is installed")
            .create_descriptor_heap
            .call(this, desc, iid, output)
    };
    if result.is_err() || desc.is_null() || output.is_null() || unsafe { (*output).is_null() } {
        return result;
    }
    let desc = unsafe { *desc };
    if desc.Type != D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV
        || !desc
            .Flags
            .contains(D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE)
    {
        return result;
    }
    let raw = unsafe { *output };
    if let Some(unknown) = (unsafe { IUnknown::from_raw_borrowed(&raw) })
        && let Ok(heap) = unknown.cast::<ID3D12DescriptorHeap>()
    {
        let (cpu, gpu) = unsafe {
            (
                heap.GetCPUDescriptorHandleForHeapStart(),
                heap.GetGPUDescriptorHandleForHeapStart(),
            )
        };
        let increment = DESCRIPTOR_INCREMENT.get().copied().unwrap_or(0);
        if gpu.ptr != 0 && increment != 0 {
            GPU_HEAPS.insert(
                gpu.ptr,
                HeapInfo {
                    cpu_base: cpu.ptr,
                    gpu_base: gpu.ptr,
                    count: desc.NumDescriptors,
                    increment,
                },
            );
        }
    }
    result
}

unsafe extern "system" fn create_shader_resource_view(
    this: *mut c_void,
    resource: *mut c_void,
    desc: *const D3D12_SHADER_RESOURCE_VIEW_DESC,
    destination: D3D12_CPU_DESCRIPTOR_HANDLE,
) {
    unsafe {
        DETOURS
            .get()
            .expect("SRV detour is installed")
            .create_shader_resource_view
            .call(this, resource, desc, destination)
    };
    if let Some(resource) = unsafe { tracked_resource(resource) } {
        CPU_DESCRIPTORS.insert(destination.ptr, resource);
    } else {
        CPU_DESCRIPTORS.remove(&destination.ptr);
    }
}

unsafe extern "system" fn create_render_target_view(
    this: *mut c_void,
    resource: *mut c_void,
    desc: *const D3D12_RENDER_TARGET_VIEW_DESC,
    destination: D3D12_CPU_DESCRIPTOR_HANDLE,
) {
    unsafe {
        DETOURS
            .get()
            .expect("RTV detour is installed")
            .create_render_target_view
            .call(this, resource, desc, destination)
    };
    if let Some(resource) = unsafe { tracked_resource(resource) } {
        RTV_DESCRIPTORS.insert(
            destination.ptr,
            TrackedTarget {
                key: identity_key(&resource.resource),
                width: resource.width,
                height: resource.height,
            },
        );
    } else {
        RTV_DESCRIPTORS.remove(&destination.ptr);
    }
}

unsafe extern "system" fn copy_descriptors_simple(
    this: *mut c_void,
    count: u32,
    destination: D3D12_CPU_DESCRIPTOR_HANDLE,
    source: D3D12_CPU_DESCRIPTOR_HANDLE,
    heap_type: D3D12_DESCRIPTOR_HEAP_TYPE,
) {
    unsafe {
        DETOURS
            .get()
            .expect("descriptor copy detour is installed")
            .copy_descriptors_simple
            .call(this, count, destination, source, heap_type)
    };
    if heap_type != D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV {
        return;
    }
    let increment = DESCRIPTOR_INCREMENT.get().copied().unwrap_or(0) as usize;
    if increment == 0 {
        return;
    }
    for index in 0..count as usize {
        let source = source.ptr + index * increment;
        let destination = destination.ptr + index * increment;
        if let Some(resource) = CPU_DESCRIPTORS.get(&source).map(|entry| entry.clone()) {
            CPU_DESCRIPTORS.insert(destination, resource);
        } else {
            CPU_DESCRIPTORS.remove(&destination);
        }
    }
}

unsafe extern "system" fn set_graphics_root_descriptor_table(
    this: *mut c_void,
    root_index: u32,
    descriptor: D3D12_GPU_DESCRIPTOR_HANDLE,
) {
    unsafe {
        DETOURS
            .get()
            .expect("root descriptor detour is installed")
            .set_graphics_root_descriptor_table
            .call(this, root_index, descriptor)
    };
    let mut state = COMMANDS.entry(this as usize).or_default();
    if let Some(resource) = resolve_gpu_descriptor(descriptor.ptr) {
        state.root_sources.insert(root_index, resource);
    } else {
        state.root_sources.remove(&root_index);
    }
}

unsafe extern "system" fn set_graphics_root_signature(this: *mut c_void, signature: *mut c_void) {
    unsafe {
        DETOURS
            .get()
            .expect("root signature detour is installed")
            .set_graphics_root_signature
            .call(this, signature)
    };
    COMMANDS
        .entry(this as usize)
        .or_default()
        .root_sources
        .clear();
}

unsafe extern "system" fn om_set_render_targets(
    this: *mut c_void,
    count: u32,
    descriptors: *const D3D12_CPU_DESCRIPTOR_HANDLE,
    contiguous: BOOL,
    depth: *const D3D12_CPU_DESCRIPTOR_HANDLE,
) {
    unsafe {
        DETOURS
            .get()
            .expect("render-target detour is installed")
            .om_set_render_targets
            .call(this, count, descriptors, contiguous, depth)
    };
    let target = (count != 0 && !descriptors.is_null())
        .then(|| unsafe { (*descriptors).ptr })
        .and_then(|handle| RTV_DESCRIPTORS.get(&handle).map(|entry| *entry));
    COMMANDS.entry(this as usize).or_default().render_target = target;
}

unsafe extern "system" fn draw_instanced(
    this: *mut c_void,
    vertex_count: u32,
    instance_count: u32,
    start_vertex: u32,
    start_instance: u32,
) {
    unsafe {
        DETOURS
            .get()
            .expect("draw detour is installed")
            .draw_instanced
            .call(
                this,
                vertex_count,
                instance_count,
                start_vertex,
                start_instance,
            )
    };
    record_draw(this);
}

unsafe extern "system" fn draw_indexed_instanced(
    this: *mut c_void,
    index_count: u32,
    instance_count: u32,
    start_index: u32,
    base_vertex: i32,
    start_instance: u32,
) {
    unsafe {
        DETOURS
            .get()
            .expect("indexed draw detour is installed")
            .draw_indexed_instanced
            .call(
                this,
                index_count,
                instance_count,
                start_index,
                base_vertex,
                start_instance,
            )
    };
    record_draw(this);
}

pub(super) fn record_draw(this: *mut c_void) {
    let mut state = COMMANDS.entry(this as usize).or_default();
    let Some(region) = state
        .scissor
        .and_then(region_from_scissor)
        .or_else(|| state.viewport.and_then(region_from_viewport))
    else {
        return;
    };
    let draw_area = u64::from(region.width) * u64::from(region.height);
    let target = state.render_target.clone();
    let source = state
        .root_sources
        .values()
        .filter(|source| {
            target
                .as_ref()
                .is_none_or(|target| identity_key(&source.resource) != target.key)
                && source.width >= 64
                && source.height >= 64
        })
        .max_by_key(|source| u64::from(source.width) * u64::from(source.height))
        .cloned();
    let target_extent = target.as_ref().map_or(
        (region.x + region.width, region.y + region.height),
        |target| (target.width, target.height),
    );
    let candidate = Candidate {
        region,
        draw_area,
        target_extent,
        source,
    };
    if state
        .candidate
        .as_ref()
        .is_none_or(|current| candidate.rank() >= current.rank())
    {
        state.candidate = Some(candidate.clone());
    }
    if let Some(target) = target {
        if state.recorded_targets.insert(target.key) {
            PRESENT_REGIONS.insert(target.key, candidate);
        }
    }
}

unsafe extern "system" fn resource_barrier(
    this: *mut c_void,
    count: u32,
    barriers: *const D3D12_RESOURCE_BARRIER,
) {
    unsafe {
        DETOURS
            .get()
            .expect("barrier detour is installed")
            .resource_barrier
            .call(this, count, barriers)
    };
    if count == 0 || barriers.is_null() {
        return;
    }
    let barriers = unsafe { std::slice::from_raw_parts(barriers, count as usize) };
    for barrier in barriers {
        if barrier.Type != D3D12_RESOURCE_BARRIER_TYPE_TRANSITION {
            continue;
        }
        let transition = unsafe { &*barrier.Anonymous.Transition };
        if let Some(resource) = transition.pResource.as_ref() {
            RESOURCE_STATES.insert(
                identity_key(resource),
                D3d12CaptureSourceState::Legacy(transition.StateAfter),
            );
        }
    }
    let Some(candidate) = COMMANDS.get(&(this as usize)).and_then(|state| {
        state
            .render_target
            .is_none()
            .then(|| state.candidate.clone())
            .flatten()
    }) else {
        return;
    };
    for barrier in barriers {
        if barrier.Type != D3D12_RESOURCE_BARRIER_TYPE_TRANSITION {
            continue;
        }
        let transition = unsafe { &*barrier.Anonymous.Transition };
        if transition.StateAfter != D3D12_RESOURCE_STATE_PRESENT {
            continue;
        }
        let Some(resource) = transition.pResource.as_ref() else {
            continue;
        };
        let key = identity_key(resource);
        let desc = unsafe { resource.GetDesc() };
        let mut candidate = candidate.clone();
        candidate.target_extent = (desc.Width as u32, desc.Height);
        record_candidate(key, candidate);
    }
}

unsafe extern "system" fn enhanced_barrier(
    this: *mut c_void,
    count: u32,
    groups: *const D3D12_BARRIER_GROUP,
) {
    unsafe {
        ENHANCED_BARRIER
            .get()
            .expect("enhanced barrier detour is installed")
            .call(this, count, groups)
    };
    if count == 0 || groups.is_null() {
        return;
    }
    let groups = unsafe { std::slice::from_raw_parts(groups, count as usize) };
    for group in groups {
        if group.Type != D3D12_BARRIER_TYPE_TEXTURE {
            continue;
        }
        let texture_barriers = unsafe { group.Anonymous.pTextureBarriers };
        if texture_barriers.is_null() {
            continue;
        }
        for barrier in
            unsafe { std::slice::from_raw_parts(texture_barriers, group.NumBarriers as usize) }
        {
            if let Some(resource) = barrier.pResource.as_ref() {
                RESOURCE_STATES.insert(
                    identity_key(resource),
                    D3d12CaptureSourceState::Enhanced {
                        sync: barrier.SyncAfter,
                        access: barrier.AccessAfter,
                        layout: barrier.LayoutAfter,
                    },
                );
            }
        }
    }
    let Some(candidate) = COMMANDS.get(&(this as usize)).and_then(|state| {
        state
            .render_target
            .is_none()
            .then(|| state.candidate.clone())
            .flatten()
    }) else {
        return;
    };
    for group in groups {
        if group.Type != D3D12_BARRIER_TYPE_TEXTURE {
            continue;
        }
        let texture_barriers = unsafe { group.Anonymous.pTextureBarriers };
        if texture_barriers.is_null() {
            continue;
        }
        for barrier in
            unsafe { std::slice::from_raw_parts(texture_barriers, group.NumBarriers as usize) }
        {
            if barrier.LayoutAfter != D3D12_BARRIER_LAYOUT_COMMON {
                continue;
            }
            let Some(resource) = barrier.pResource.as_ref() else {
                continue;
            };
            let desc = unsafe { resource.GetDesc() };
            let mut candidate = candidate.clone();
            candidate.target_extent = (desc.Width as u32, desc.Height);
            record_candidate(identity_key(resource), candidate);
        }
    }
}

pub(crate) unsafe fn swapchain_presentation(swapchain: *mut c_void) -> Option<Presentation> {
    std::env::var_os("SNOWGLASS_TRACK_RENDER_TARGET")?;
    let swapchain = unsafe { IDXGISwapChain3::from_raw_borrowed(&swapchain) }?;
    let index = unsafe { swapchain.GetCurrentBackBufferIndex() };
    let resource: ID3D12Resource = unsafe { swapchain.GetBuffer(index).ok()? };
    let candidate = PRESENT_REGIONS.get(&identity_key(&resource))?;
    let region_is_content = (candidate.region.x, candidate.region.y) != (0, 0)
        || (candidate.region.width, candidate.region.height) != candidate.target_extent;
    let source = (!region_is_content)
        .then(|| candidate.source.clone())
        .flatten()
        .map(|mut source| {
            source.state = RESOURCE_STATES
                .get(&identity_key(&source.resource))
                .map(|entry| *entry);
            source
        });
    Some(Presentation {
        region: region_is_content.then_some(candidate.region),
        source,
    })
}

fn record_candidate(key: usize, candidate: Candidate) {
    if PRESENT_REGIONS
        .get(&key)
        .is_none_or(|current| candidate.rank() >= current.rank())
    {
        PRESENT_REGIONS.insert(key, candidate);
    }
}

unsafe fn tracked_resource(raw: *mut c_void) -> Option<TrackedResource> {
    let resource = unsafe { ID3D12Resource::from_raw_borrowed(&raw) }?.clone();
    let desc = unsafe { resource.GetDesc() };
    Some(TrackedResource {
        resource,
        width: desc.Width.try_into().ok()?,
        height: desc.Height,
        format: desc.Format.0 as u32,
        state: None,
    })
}

fn resolve_gpu_descriptor(handle: u64) -> Option<TrackedResource> {
    for heap in GPU_HEAPS.iter() {
        let info = heap.value();
        let size = u64::from(info.count) * u64::from(info.increment);
        if handle < info.gpu_base || handle >= info.gpu_base.saturating_add(size) {
            continue;
        }
        let offset = handle - info.gpu_base;
        if offset % u64::from(info.increment) != 0 {
            continue;
        }
        let cpu = info.cpu_base + offset as usize;
        return CPU_DESCRIPTORS.get(&cpu).map(|entry| entry.clone());
    }
    None
}

fn region_from_scissor(scissor: RECT) -> Option<D3d12CaptureRegion> {
    let left = scissor.left.max(0) as u32;
    let top = scissor.top.max(0) as u32;
    let right = scissor.right.max(0) as u32;
    let bottom = scissor.bottom.max(0) as u32;
    (right > left && bottom > top).then_some(D3d12CaptureRegion {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

fn region_from_viewport(viewport: D3D12_VIEWPORT) -> Option<D3d12CaptureRegion> {
    let left = viewport.TopLeftX.max(0.0).round() as u32;
    let top = viewport.TopLeftY.max(0.0).round() as u32;
    let width = viewport.Width.abs().round() as u32;
    let height = viewport.Height.abs().round() as u32;
    (width != 0 && height != 0).then_some(D3d12CaptureRegion {
        x: left,
        y: top,
        width,
        height,
    })
}

fn identity_key(resource: &ID3D12Resource) -> usize {
    resource
        .cast::<IUnknown>()
        .map_or(resource.as_raw() as usize, |identity| {
            identity.as_raw() as usize
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scissor_is_used_without_intersecting_the_viewport() {
        assert_eq!(
            region_from_scissor(RECT {
                left: 160,
                top: 90,
                right: 1760,
                bottom: 990,
            }),
            Some(D3d12CaptureRegion {
                x: 160,
                y: 90,
                width: 1600,
                height: 900,
            })
        );
    }
}
