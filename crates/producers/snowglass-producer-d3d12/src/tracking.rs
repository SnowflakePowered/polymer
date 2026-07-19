//! Capture-region tracking derived from D3D12 command-list state.
//!
//! The vtable detours that drive this live in the D3D12 hook crate, which calls
//! the `record_*` observers below after forwarding each intercepted method. This
//! module owns the tracking state: descriptor bookkeeping, per-command-list
//! draw candidates, resource states, and the presentation lookup consumed at
//! capture time.

use crate::{D3d12CaptureRegion, D3d12CaptureSourceState};
use dashmap::DashMap;
use std::{
    collections::{HashMap, HashSet},
    ffi::c_void,
    sync::OnceLock,
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
    core::{IUnknown, Interface},
};

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

/// The presentation source resolved for a swapchain backbuffer.
pub struct Presentation {
    pub region: Option<D3d12CaptureRegion>,
    pub source: Option<TrackedResource>,
    pub target_extent: (u32, u32),
}

/// A render target tracked as a candidate capture source.
#[derive(Clone)]
pub struct TrackedResource {
    pub resource: ID3D12Resource,
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub state: Option<D3d12CaptureSourceState>,
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

/// Device-scoped state shared by the D3D12 hook contexts.
#[derive(Default)]
pub struct Tracking {
    commands: DashMap<usize, CommandState>,
    present_regions: DashMap<usize, Candidate>,
    cpu_descriptors: DashMap<usize, TrackedResource>,
    rtv_descriptors: DashMap<usize, TrackedTarget>,
    resource_states: DashMap<usize, D3d12CaptureSourceState>,
    gpu_heaps: DashMap<u64, HeapInfo>,
    descriptor_increment: OnceLock<u32>,
    enhanced_barriers_supported: OnceLock<bool>,
    features_probed: OnceLock<()>,
}

impl Tracking {
    pub fn enhanced_barriers_supported(&self) -> bool {
        self.enhanced_barriers_supported
            .get()
            .copied()
            .unwrap_or(false)
    }

    /// Detects capture-related features for this device context: whether enhanced
    /// barriers can be recorded, and the shader-visible descriptor stride.
    ///
    /// Returns the enhanced-barrier decision on the first probe, and `None` once
    /// the features have already been recorded.
    pub fn detect_device_features(&self, device: &ID3D12Device) -> Result<Option<bool>, String> {
        if self.features_probed.get().is_some() {
            return Ok(None);
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
        let _ = self
            .enhanced_barriers_supported
            .set(enhanced_barriers_supported);
        let _ = self.descriptor_increment.set(unsafe {
            device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV)
        });
        let _ = self.features_probed.set(());
        Ok(Some(enhanced_barriers_supported))
    }

    /// Releases every tracker-owned reference to the swapchain's current buffers.
    ///
    /// DXGI requires all direct and indirect references obtained through
    /// `GetBuffer` to be gone before `ResizeBuffers` is called. The tracking maps
    /// deliberately retain resources across command-list execution, so their
    /// swapchain entries must be invalidated explicitly at this lifecycle boundary.
    ///
    /// # Safety
    ///
    /// `swapchain` must be null or point to a live `IDXGISwapChain3`.
    pub unsafe fn before_swapchain_resize(&self, swapchain: *mut c_void) {
        let Some(swapchain) = (unsafe { IDXGISwapChain3::from_raw_borrowed(&swapchain) }) else {
            return;
        };
        let Ok(desc) = (unsafe { swapchain.GetDesc() }) else {
            return;
        };
        let buffer_keys = (0..desc.BufferCount)
            .filter_map(|index| unsafe { swapchain.GetBuffer::<ID3D12Resource>(index).ok() })
            .map(|resource| Self::identity_key(&resource))
            .collect::<HashSet<_>>();
        if buffer_keys.is_empty() {
            return;
        }

        self.cpu_descriptors
            .retain(|_, resource| !buffer_keys.contains(&Self::identity_key(&resource.resource)));
        self.rtv_descriptors
            .retain(|_, target| !buffer_keys.contains(&target.key));
        self.present_regions.retain(|key, candidate| {
            !buffer_keys.contains(key)
                && candidate.source.as_ref().is_none_or(|source| {
                    !buffer_keys.contains(&Self::identity_key(&source.resource))
                })
        });
        self.resource_states
            .retain(|key, _| !buffer_keys.contains(key));
        for mut command in self.commands.iter_mut() {
            let state = command.value_mut();
            state
                .root_sources
                .retain(|_, source| !buffer_keys.contains(&Self::identity_key(&source.resource)));
            if state
                .candidate
                .as_ref()
                .and_then(|candidate| candidate.source.as_ref())
                .is_some_and(|source| buffer_keys.contains(&Self::identity_key(&source.resource)))
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

    /// Discards the tracked state for a command list being reset.
    pub fn command_reset(&self, list: *mut c_void) {
        self.commands.remove(&(list as usize));
    }

    /// Records the viewport set on a command list.
    ///
    /// # Safety
    ///
    /// `viewports` must be null or reference `count` valid `D3D12_VIEWPORT` values.
    pub unsafe fn record_viewports(
        &self,
        list: *mut c_void,
        count: u32,
        viewports: *const D3D12_VIEWPORT,
    ) {
        if count != 0 && !viewports.is_null() {
            self.commands.entry(list as usize).or_default().viewport = Some(unsafe { *viewports });
        }
    }

    /// Records the scissor rect set on a command list.
    ///
    /// # Safety
    ///
    /// `scissors` must be null or reference `count` valid `RECT` values.
    pub unsafe fn record_scissors(&self, list: *mut c_void, count: u32, scissors: *const RECT) {
        if count != 0 && !scissors.is_null() {
            self.commands.entry(list as usize).or_default().scissor = Some(unsafe { *scissors });
        }
    }

    /// Records a newly created shader-visible descriptor heap.
    ///
    /// # Safety
    ///
    /// `desc` must be null or reference a valid heap description, and `output` must
    /// be null or point to the created `ID3D12DescriptorHeap` COM pointer.
    pub unsafe fn record_descriptor_heap(
        &self,
        desc: *const D3D12_DESCRIPTOR_HEAP_DESC,
        output: *mut *mut c_void,
    ) {
        if desc.is_null() || output.is_null() || unsafe { (*output).is_null() } {
            return;
        }
        let desc = unsafe { *desc };
        if desc.Type != D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV
            || !desc
                .Flags
                .contains(D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE)
        {
            return;
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
            let increment = self.descriptor_increment.get().copied().unwrap_or(0);
            if gpu.ptr != 0 && increment != 0 {
                self.gpu_heaps.insert(
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
    }

    /// Records the resource backing a newly created shader resource view.
    ///
    /// # Safety
    ///
    /// `resource` must be null or point to a live `ID3D12Resource`; `desc` is
    /// unused and may be null.
    pub unsafe fn record_shader_resource_view(
        &self,
        resource: *mut c_void,
        _desc: *const D3D12_SHADER_RESOURCE_VIEW_DESC,
        destination: D3D12_CPU_DESCRIPTOR_HANDLE,
    ) {
        if let Some(resource) = unsafe { Self::tracked_resource(resource) } {
            self.cpu_descriptors.insert(destination.ptr, resource);
        } else {
            self.cpu_descriptors.remove(&destination.ptr);
        }
    }

    /// Records the resource backing a newly created render target view.
    ///
    /// # Safety
    ///
    /// `resource` must be null or point to a live `ID3D12Resource`; `desc` is
    /// unused and may be null.
    pub unsafe fn record_render_target_view(
        &self,
        resource: *mut c_void,
        _desc: *const D3D12_RENDER_TARGET_VIEW_DESC,
        destination: D3D12_CPU_DESCRIPTOR_HANDLE,
    ) {
        if let Some(resource) = unsafe { Self::tracked_resource(resource) } {
            self.rtv_descriptors.insert(
                destination.ptr,
                TrackedTarget {
                    key: Self::identity_key(&resource.resource),
                    width: resource.width,
                    height: resource.height,
                },
            );
        } else {
            self.rtv_descriptors.remove(&destination.ptr);
        }
    }

    /// Propagates descriptor bindings across a `CopyDescriptorsSimple`.
    pub fn record_copy_descriptors_simple(
        &self,
        count: u32,
        destination: D3D12_CPU_DESCRIPTOR_HANDLE,
        source: D3D12_CPU_DESCRIPTOR_HANDLE,
        heap_type: D3D12_DESCRIPTOR_HEAP_TYPE,
    ) {
        if heap_type != D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV {
            return;
        }
        let increment = self.descriptor_increment.get().copied().unwrap_or(0) as usize;
        if increment == 0 {
            return;
        }
        for index in 0..count as usize {
            let source = source.ptr + index * increment;
            let destination = destination.ptr + index * increment;
            if let Some(resource) = self.cpu_descriptors.get(&source).map(|entry| entry.clone()) {
                self.cpu_descriptors.insert(destination, resource);
            } else {
                self.cpu_descriptors.remove(&destination);
            }
        }
    }

    /// Records the resource bound to a graphics root descriptor table.
    pub fn record_root_descriptor_table(
        &self,
        list: *mut c_void,
        root_index: u32,
        descriptor: D3D12_GPU_DESCRIPTOR_HANDLE,
    ) {
        let mut state = self.commands.entry(list as usize).or_default();
        if let Some(resource) = self.resolve_gpu_descriptor(descriptor.ptr) {
            state.root_sources.insert(root_index, resource);
        } else {
            state.root_sources.remove(&root_index);
        }
    }

    /// Clears the tracked root bindings when a new root signature is set.
    pub fn record_root_signature(&self, list: *mut c_void) {
        self.commands
            .entry(list as usize)
            .or_default()
            .root_sources
            .clear();
    }

    /// Records the render target bound on a command list.
    ///
    /// # Safety
    ///
    /// `descriptors` must be null or reference `count` valid handles.
    pub unsafe fn record_render_targets(
        &self,
        list: *mut c_void,
        count: u32,
        descriptors: *const D3D12_CPU_DESCRIPTOR_HANDLE,
    ) {
        let target = (count != 0 && !descriptors.is_null())
            .then(|| unsafe { (*descriptors).ptr })
            .and_then(|handle| self.rtv_descriptors.get(&handle).map(|entry| *entry));
        self.commands
            .entry(list as usize)
            .or_default()
            .render_target = target;
    }

    /// Records the render region in effect at a draw on the given command list.
    pub fn record_draw(&self, list: *mut c_void) {
        let mut state = self.commands.entry(list as usize).or_default();
        let Some(region) = state
            .scissor
            .and_then(Self::region_from_scissor)
            .or_else(|| state.viewport.and_then(Self::region_from_viewport))
        else {
            return;
        };
        let draw_area = u64::from(region.width) * u64::from(region.height);
        let target = state.render_target;
        let source = state
            .root_sources
            .values()
            .filter(|source| {
                target
                    .as_ref()
                    .is_none_or(|target| Self::identity_key(&source.resource) != target.key)
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
        if let Some(target) = target
            && state.recorded_targets.insert(target.key)
        {
            self.present_regions.insert(target.key, candidate);
        }
    }

    /// Records legacy resource-state transitions and present-time candidates.
    ///
    /// # Safety
    ///
    /// `barriers` must be null or reference `count` valid barriers.
    pub unsafe fn record_resource_barrier(
        &self,
        list: *mut c_void,
        count: u32,
        barriers: *const D3D12_RESOURCE_BARRIER,
    ) {
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
                self.resource_states.insert(
                    Self::identity_key(resource),
                    D3d12CaptureSourceState::Legacy(transition.StateAfter),
                );
            }
        }
        let Some(candidate) = self.commands.get(&(list as usize)).and_then(|state| {
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
            let key = Self::identity_key(resource);
            let desc = unsafe { resource.GetDesc() };
            let mut candidate = candidate.clone();
            candidate.target_extent = (desc.Width as u32, desc.Height);
            self.record_candidate(key, candidate);
        }
    }

    /// Records enhanced-barrier resource states and present-time candidates.
    ///
    /// # Safety
    ///
    /// `groups` must be null or reference `count` valid barrier groups.
    pub unsafe fn record_enhanced_barrier(
        &self,
        list: *mut c_void,
        count: u32,
        groups: *const D3D12_BARRIER_GROUP,
    ) {
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
                    self.resource_states.insert(
                        Self::identity_key(resource),
                        D3d12CaptureSourceState::Enhanced {
                            sync: barrier.SyncAfter,
                            access: barrier.AccessAfter,
                            layout: barrier.LayoutAfter,
                        },
                    );
                }
            }
        }
        let Some(candidate) = self.commands.get(&(list as usize)).and_then(|state| {
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
                self.record_candidate(Self::identity_key(resource), candidate);
            }
        }
    }

    /// Resolves the presentation source recorded for a swapchain's current
    /// backbuffer.
    ///
    /// # Safety
    ///
    /// `swapchain` must be null or point to a live `IDXGISwapChain3`.
    pub unsafe fn swapchain_presentation(&self, swapchain: *mut c_void) -> Option<Presentation> {
        snowglass_hook::config::tracking_enabled().then_some(())?;
        let swapchain = unsafe { IDXGISwapChain3::from_raw_borrowed(&swapchain) }?;
        let index = unsafe { swapchain.GetCurrentBackBufferIndex() };
        let resource: ID3D12Resource = unsafe { swapchain.GetBuffer(index).ok()? };
        let candidate = self.present_regions.get(&Self::identity_key(&resource))?;
        let region_is_content = (candidate.region.x, candidate.region.y) != (0, 0)
            || (candidate.region.width, candidate.region.height) != candidate.target_extent;
        let source = (!region_is_content)
            .then(|| candidate.source.clone())
            .flatten()
            .map(|mut source| {
                source.state = self
                    .resource_states
                    .get(&Self::identity_key(&source.resource))
                    .map(|entry| *entry);
                source
            });
        Some(Presentation {
            region: region_is_content.then_some(candidate.region),
            source,
            target_extent: candidate.target_extent,
        })
    }

    fn record_candidate(&self, key: usize, candidate: Candidate) {
        if self
            .present_regions
            .get(&key)
            .is_none_or(|current| candidate.rank() >= current.rank())
        {
            self.present_regions.insert(key, candidate);
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

    fn resolve_gpu_descriptor(&self, handle: u64) -> Option<TrackedResource> {
        for heap in self.gpu_heaps.iter() {
            let info = heap.value();
            let size = u64::from(info.count) * u64::from(info.increment);
            if handle < info.gpu_base || handle >= info.gpu_base.saturating_add(size) {
                continue;
            }
            let offset = handle - info.gpu_base;
            if !offset.is_multiple_of(u64::from(info.increment)) {
                continue;
            }
            let cpu = info.cpu_base + offset as usize;
            return self.cpu_descriptors.get(&cpu).map(|entry| entry.clone());
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scissor_is_used_without_intersecting_the_viewport() {
        assert_eq!(
            Tracking::region_from_scissor(RECT {
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

    #[test]
    fn tracking_instances_do_not_share_command_state() {
        let first = Tracking::default();
        let second = Tracking::default();
        let viewport = D3D12_VIEWPORT {
            Width: 1280.0,
            Height: 720.0,
            ..Default::default()
        };

        unsafe { first.record_viewports(1usize as *mut c_void, 1, &raw const viewport) };

        assert!(first.commands.contains_key(&1));
        assert!(!second.commands.contains_key(&1));
    }
}
