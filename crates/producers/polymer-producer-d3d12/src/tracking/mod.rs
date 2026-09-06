//! Capture-region tracking derived from D3D12 command-list state.
//!
//! The vtable detours that drive this live in the D3D12 hook crate, which calls
//! the `record_*` observers below after forwarding each intercepted method. This
//! module owns the tracking state: descriptor bookkeeping, per-command-list
//! draw candidates, resource states, and the presentation lookup consumed at
//! capture time.

mod barriers;
mod bundles;
use bundles::Bindings;
mod clears;
mod copies;
mod descriptors;

use crate::{D3d12CaptureRegion, D3d12CaptureSourceState};
use dashmap::DashMap;
use polymer_presentation::{
    DependencyEdge, Extent2D, GraphSnapshot, ObservationStore, PresentationSelector, Surface,
    Target,
};
use std::{
    collections::{HashMap, HashSet},
    ffi::c_void,
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};
use windows::{
    Win32::{
        Foundation::RECT,
        Graphics::{
            Direct3D12::{
                D3D12_COMMAND_LIST_TYPE_DIRECT, D3D12_CPU_DESCRIPTOR_HANDLE,
                D3D12_DESCRIPTOR_HEAP_DESC, D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                D3D12_DESCRIPTOR_HEAP_TYPE, D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                D3D12_FEATURE_D3D12_OPTIONS12, D3D12_FEATURE_DATA_D3D12_OPTIONS12,
                D3D12_GPU_DESCRIPTOR_HANDLE, D3D12_RENDER_TARGET_VIEW_DESC,
                D3D12_SHADER_RESOURCE_VIEW_DESC, D3D12_VIEWPORT, ID3D12CommandAllocator,
                ID3D12DescriptorHeap, ID3D12Device, ID3D12GraphicsCommandList,
                ID3D12GraphicsCommandList7, ID3D12PipelineState, ID3D12Resource,
            },
            Dxgi::IDXGISwapChain3,
        },
    },
    core::{IUnknown, Interface},
};

type SurfaceKey = (usize, u32);

#[derive(Clone)]
struct Candidate {
    evidence: polymer_presentation::Evidence,
    region: D3d12CaptureRegion,
    draw_area: u64,
    source: Option<TrackedResource>,
    mapping: Option<polymer_presentation::RegionMapping>,
}

/// The presentation source resolved for a swapchain backbuffer.
#[derive(Clone)]
pub struct Presentation {
    pub exact_selection: bool,
    pub placement_known: bool,
    pub region: Option<D3d12CaptureRegion>,
    pub source_region: Option<D3d12CaptureRegion>,
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
    pub sample_count: u32,
    pub subresource: u32,
    mip_layer: (u32, u32),
}

#[derive(Clone, Copy)]
struct TrackedTarget {
    key: SurfaceKey,
    width: u32,
    height: u32,
}

#[derive(Default, Clone)]
struct CommandState {
    inherited_draws: Vec<InheritedDraw>,
    viewport_set: bool,
    scissor_set: bool,
    viewport: Option<D3D12_VIEWPORT>,
    scissor: Option<RECT>,
    render_target: Option<TrackedTarget>,
    render_targets: Vec<TrackedTarget>,
    bindings: Bindings,
    recorded_targets: HashSet<SurfaceKey>,
    observations: Vec<(SurfaceKey, Candidate)>,
    draw_tables: HashMap<usize, Bindings>,
    transitions: Vec<(SurfaceKey, Option<D3d12CaptureSourceState>)>,
}

#[derive(Clone)]
struct InheritedDraw {
    viewport: Option<D3D12_VIEWPORT>,
    scissor: Option<RECT>,
    bindings: Bindings,
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
    pub(crate) registry: polymer_presentation::registry::PresentationRegistry,
    pub(crate) presentation_clock: AtomicU64,
    commands: DashMap<usize, CommandState>,
    observations: ObservationStore<SurfaceKey>,
    resources: DashMap<SurfaceKey, (TrackedResource, u64)>,
    generation: AtomicU64,
    observation_order: AtomicU64,
    cpu_descriptors: DashMap<usize, Vec<TrackedResource>>,
    rtv_descriptors: DashMap<usize, Vec<TrackedTarget>>,
    resource_states: DashMap<SurfaceKey, Option<D3d12CaptureSourceState>>,
    resource_queues: DashMap<SurfaceKey, usize>,
    gpu_heaps: DashMap<u64, HeapInfo>,
    root_signatures: DashMap<usize, Vec<Vec<(u32, u32)>>>,
    descriptor_increment: OnceLock<u32>,
    rtv_increment: OnceLock<u32>,
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
        let _ = self.rtv_increment.set(unsafe {
            device.GetDescriptorHandleIncrementSize(
                windows::Win32::Graphics::Direct3D12::D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
            )
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

        let _observations = self.observations.transaction();
        self.observations.clear();
        self.resources.clear();
        self.cpu_descriptors.retain(|_, resources| {
            resources
                .retain(|resource| !buffer_keys.contains(&Self::identity_key(&resource.resource)));
            !resources.is_empty()
        });
        self.rtv_descriptors.retain(|_, targets| {
            targets.retain(|target| !buffer_keys.contains(&target.key.0));
            !targets.is_empty()
        });
        self.resource_states
            .retain(|key, _| !buffer_keys.contains(&key.0));
        for mut command in self.commands.iter_mut() {
            let state = command.value_mut();
            state
                .render_targets
                .retain(|target| !buffer_keys.contains(&target.key.0));
            state.observations.clear();
            state.draw_tables.clear();
            state.transitions.clear();
            state.bindings.tables.clear();
            if state
                .render_target
                .is_some_and(|target| buffer_keys.contains(&target.key.0))
            {
                state.render_target = None;
            }
            state
                .recorded_targets
                .retain(|key| !buffer_keys.contains(&key.0));
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
        let mut state = self.commands.entry(list as usize).or_default();
        state.viewport_set = true;
        state.viewport = (count > 0 && !viewports.is_null()).then(|| unsafe { *viewports });
    }

    /// Records the scissor rect set on a command list.
    ///
    /// # Safety
    ///
    /// `scissors` must be null or reference `count` valid `RECT` values.
    pub unsafe fn record_scissors(&self, list: *mut c_void, count: u32, scissors: *const RECT) {
        let mut state = self.commands.entry(list as usize).or_default();
        state.scissor_set = true;
        state.scissor = (count > 0 && !scissors.is_null()).then(|| unsafe { *scissors });
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
        let raw = unsafe { *output };
        if let Some(unknown) = (unsafe { IUnknown::from_raw_borrowed(&raw) })
            && let Ok(heap) = unknown.cast::<ID3D12DescriptorHeap>()
        {
            let cpu = unsafe { heap.GetCPUDescriptorHandleForHeapStart() };
            self.reset_descriptor_range(cpu.ptr, desc.NumDescriptors, desc.Type);
            if desc.Type != D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV
                || !desc
                    .Flags
                    .contains(D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE)
            {
                return;
            }
            let gpu = unsafe { heap.GetGPUDescriptorHandleForHeapStart() };
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
        let resources = unsafe { Self::tracked_resource(resource) }
            .map(|resource| resource.shader_views(unsafe { _desc.as_ref() }))
            .unwrap_or_default();
        if resources.is_empty() {
            self.cpu_descriptors.remove(&destination.ptr);
        } else {
            self.cpu_descriptors.insert(destination.ptr, resources);
        }
    }

    /// # Safety
    /// Resource and optional descriptor follow CreateRenderTargetView.
    pub unsafe fn record_render_target_view(
        &self,
        resource: *mut c_void,
        desc: *const D3D12_RENDER_TARGET_VIEW_DESC,
        destination: D3D12_CPU_DESCRIPTOR_HANDLE,
    ) {
        let resources = unsafe { Self::tracked_resource(resource) }
            .map(|resource| resource.render_views(unsafe { desc.as_ref() }))
            .unwrap_or_default();
        let targets = resources
            .into_iter()
            .map(|resource| TrackedTarget {
                key: (Self::identity_key(&resource.resource), resource.subresource),
                width: resource.width,
                height: resource.height,
            })
            .collect::<Vec<_>>();
        if targets.is_empty() {
            self.rtv_descriptors.remove(&destination.ptr);
        } else {
            self.rtv_descriptors.insert(destination.ptr, targets);
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
        unsafe {
            self.copy_descriptors(1, &destination, &count, 1, &source, &count, heap_type);
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
        state.bindings.tables.insert(root_index, descriptor.ptr);
    }

    /// Updates root layout and invalidates tables only when the signature changes.
    pub fn record_root_signature(&self, list: *mut c_void, signature: *mut c_void) {
        let mut state = self.commands.entry(list as usize).or_default();
        state.bindings.set_signature(signature as usize);
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
        contiguous: bool,
    ) {
        let increment = self.rtv_increment.get().copied().unwrap_or(0) as usize;
        let mut targets = Vec::new();
        if !descriptors.is_null() && (!contiguous || increment != 0) {
            for index in 0..count.min(8) as usize {
                let handle = if contiguous {
                    (unsafe { (*descriptors).ptr }) + index * increment
                } else {
                    unsafe { (*descriptors.add(index)).ptr }
                };
                if let Some(target) = self.rtv_descriptors.get(&handle) {
                    targets.extend(target.iter().copied());
                }
            }
        }
        let mut state = self.commands.entry(list as usize).or_default();
        state.render_target = targets.first().copied();
        state.render_targets = targets;
    }

    /// Records the render region in effect at a draw on the given command list.
    pub fn record_draw(&self, list: *mut c_void) {
        if polymer_presentation::observation::CaptureScope::active() {
            return;
        }
        let mut state = self.commands.entry(list as usize).or_default();
        if (state.viewport_set && state.viewport.is_none())
            || (state.scissor_set && state.scissor.is_none())
        {
            return;
        }
        if state.render_targets.is_empty() {
            let draw = InheritedDraw {
                viewport: state.viewport,
                scissor: state.scissor,
                bindings: state.bindings.clone(),
            };
            state.inherited_draws.push(draw);
        }
        let Some(region) = state
            .scissor
            .and_then(Self::region_from_scissor)
            .or_else(|| state.viewport.and_then(Self::region_from_viewport))
        else {
            return;
        };
        // Descriptor contents can change after recording. Keep immutable root
        // bindings here and resolve their descriptors in queue submission order.
        let tables = state.bindings.clone();
        let start = state.observations.len();
        let sources = Vec::new();
        let targets = state.render_targets.clone();
        for target in targets {
            Self::draw_target(&mut state, target, region, &sources);
        }
        for index in start..state.observations.len() {
            state.draw_tables.insert(index, tables.clone());
        }
    }

    fn draw_target(
        state: &mut CommandState,
        target: TrackedTarget,
        region: D3d12CaptureRegion,
        sources: &[TrackedResource],
    ) {
        let Some(coverage) = PresentationSelector::observed_content(
            Extent2D {
                width: target.width,
                height: target.height,
            },
            [(
                0,
                polymer_presentation::Rect {
                    x: region.x as i32,
                    y: region.y as i32,
                    extent: Extent2D {
                        width: region.width,
                        height: region.height,
                    },
                },
            )],
        ) else {
            return;
        };
        let sources = sources
            .iter()
            .filter(|source| {
                (Self::identity_key(&source.resource), source.subresource) != target.key
            })
            .cloned()
            .collect::<Vec<_>>();
        let source = sources.first().cloned();
        let candidate = Candidate {
            evidence: polymer_presentation::Evidence::SampledDraw,
            region: D3d12CaptureRegion {
                x: coverage.x as u32,
                y: coverage.y as u32,
                width: coverage.extent.width,
                height: coverage.extent.height,
            },
            draw_area: coverage.extent.area(),
            source,
            mapping: None,
        };
        state.recorded_targets.insert(target.key);
        for source in &sources {
            let mut observation = candidate.clone();
            observation.source = Some(source.clone());
            state.observations.push((target.key, observation));
        }
        if sources.is_empty() {
            state.observations.push((target.key, candidate));
        }
    }

    /// Resolves the presentation source recorded for a swapchain's current
    /// backbuffer.
    ///
    /// # Safety
    ///
    /// `swapchain` must be null or point to a live `IDXGISwapChain3`.
    pub unsafe fn swapchain_presentations(
        &self,
        swapchain: *mut c_void,
        limit: usize,
        queue: usize,
    ) -> Vec<Presentation> {
        let Some(swapchain) = (unsafe { IDXGISwapChain3::from_raw_borrowed(&swapchain) }) else {
            return Vec::new();
        };
        let index = unsafe { swapchain.GetCurrentBackBufferIndex() };
        let Ok(resource) = (unsafe { swapchain.GetBuffer::<ID3D12Resource>(index) }) else {
            return Vec::new();
        };
        let Some(presented_resource) = (unsafe { Self::tracked_resource(resource.as_raw()) })
        else {
            return Vec::new();
        };
        let presented = (Self::identity_key(&resource), 0);
        let extent = (presented_resource.width, presented_resource.height);
        let _observations = self.observations.transaction();
        let now = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let (targets, edges) = self.observations.snapshot(now);
        self.resources
            .retain(|_, (_, seen)| *seen >= now.saturating_sub(8));
        let mut graph = GraphSnapshot::default();
        for entry in self.resources.iter() {
            let source = &entry.value().0;
            graph.resources.insert(
                *entry.key(),
                ((entry.key().0, 0), source.mip_layer.0, source.mip_layer.1),
            );
            graph.surfaces.insert(*entry.key(), source.clone());
        }
        graph.surfaces.insert(presented, presented_resource);
        graph.targets = targets;
        for edge in edges {
            graph.edges.entry(edge.destination).or_default().push(edge);
        }
        let graph_tracking = polymer_hook::config::graph_tracking_enabled();
        let result = PresentationSelector::select(&graph, presented, limit, graph_tracking);
        let presentations = result
            .surfaces
            .into_iter()
            .map(|selected| {
                let source = (selected.surface != presented)
                    .then(|| graph.surfaces.get(&selected.surface).cloned())
                    .flatten()
                    .map(|mut source| {
                        source.state =
                            self.capture_state((selected.surface.0, source.subresource), queue);
                        source
                    });
                Presentation {
                    exact_selection: result.exact,
                    placement_known: selected.presentation_rect.is_some(),
                    source_region: Some(D3d12CaptureRegion {
                        x: selected.source_rect.x.max(0) as u32,
                        y: selected.source_rect.y.max(0) as u32,
                        width: selected.source_rect.extent.width,
                        height: selected.source_rect.extent.height,
                    }),
                    region: selected.presentation_rect.map(|rect| D3d12CaptureRegion {
                        x: rect.x.max(0) as u32,
                        y: rect.y.max(0) as u32,
                        width: rect.extent.width,
                        height: rect.extent.height,
                    }),
                    source,
                    target_extent: extent,
                }
            })
            .collect();
        if !graph_tracking {
            self.observations.clear();
            self.resources.clear();
        }
        presentations
    }

    /// Serializes native submission and observation publication as one transaction.
    /// # Safety
    /// `lists` references `count` live command-list interface pointers.
    pub unsafe fn execute(
        &self,
        queue: usize,
        count: u32,
        lists: *const *mut c_void,
        submit: impl FnOnce(),
    ) {
        if polymer_presentation::observation::CaptureScope::active() || lists.is_null() {
            submit();
            return;
        }
        let _observations = self.observations.transaction();
        submit();
        for &list in unsafe { std::slice::from_raw_parts(lists, count as usize) } {
            self.commit_list(list as usize, queue);
        }
    }

    fn commit_list(&self, list: usize, queue: usize) {
        let Some(state) = self.commands.get(&list) else {
            return;
        };
        for &(key, transition) in &state.transitions {
            if key.1 == u32::MAX {
                self.resource_states.retain(|old, _| old.0 != key.0);
                self.resource_queues.retain(|old, _| old.0 != key.0);
            }
            self.resource_states.insert(key, transition);
            self.resource_queues.insert(key, queue);
        }
        for (index, (key, candidate)) in state.observations.iter().enumerate() {
            let sources = state
                .draw_tables
                .get(&index)
                .map(|bindings| {
                    bindings
                        .tables
                        .iter()
                        .flat_map(|(&index, &handle)| {
                            self.table_sources(
                                bindings.signature.unwrap_or(0),
                                index,
                                handle,
                                bindings.heaps.as_deref().unwrap_or(&[]),
                            )
                        })
                        .filter(|source| {
                            (Self::identity_key(&source.resource), source.subresource) != *key
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if sources.is_empty() {
                self.record_candidate(*key, candidate.clone());
            }
            for source in sources {
                self.record_candidate(
                    *key,
                    Candidate {
                        source: Some(source),
                        ..candidate.clone()
                    },
                );
            }
        }
    }

    fn record_candidate(&self, key: SurfaceKey, candidate: Candidate) {
        let now = self.generation.load(Ordering::Relaxed);
        let order = self.observation_order.fetch_add(1, Ordering::Relaxed);
        if matches!(
            candidate.evidence,
            polymer_presentation::Evidence::SampledDraw
                | polymer_presentation::Evidence::RecoveredQuad
                | polymer_presentation::Evidence::Clear
        ) {
            self.observations.target(
                Target {
                    id: key,
                    score: candidate.draw_area,
                    order,
                },
                now,
            );
        }
        let source = candidate.source.as_ref().map_or(key, |source| {
            (Self::identity_key(&source.resource), source.subresource)
        });
        if let Some(resource) = &candidate.source {
            self.resources.insert(source, (resource.clone(), now));
        }
        let mut edge = DependencyEdge::sampled(source, key, now);
        edge.order = order;
        edge.coverage = Some(polymer_presentation::Rect {
            x: candidate.region.x as i32,
            y: candidate.region.y as i32,
            extent: Extent2D {
                width: candidate.region.width,
                height: candidate.region.height,
            },
        });
        edge.mapping = candidate.mapping;
        edge.evidence = candidate.evidence;
        self.observations.record(edge);
    }

    unsafe fn tracked_resource(raw: *mut c_void) -> Option<TrackedResource> {
        let resource = unsafe { ID3D12Resource::from_raw_borrowed(&raw) }?.clone();
        let desc = unsafe { resource.GetDesc() };
        if desc.Dimension
            != windows::Win32::Graphics::Direct3D12::D3D12_RESOURCE_DIMENSION_TEXTURE2D
        {
            return None;
        }
        Some(TrackedResource {
            resource,
            width: desc.Width.try_into().ok()?,
            height: desc.Height,
            format: desc.Format.0 as u32,
            state: None,
            subresource: 0,
            mip_layer: (0, 0),
            sample_count: desc.SampleDesc.Count,
        })
    }

    fn resolve_gpu_descriptor(&self, handle: u64) -> Option<Vec<TrackedResource>> {
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
mod tests;

impl Surface for TrackedResource {
    fn extent(&self) -> Option<Extent2D> {
        Some(Extent2D {
            width: self.width,
            height: self.height,
        })
    }
}

impl polymer_presentation::history::HistorySurface for Presentation {
    type Id = (usize, u32);
    type Layout = (u32, u32, u32, (u32, u32));
    fn identity(&self) -> Self::Id {
        self.source.as_ref().map_or((0, 0), |source| {
            (source.resource.as_raw() as usize, source.subresource)
        })
    }
    fn layout(&self) -> Self::Layout {
        self.source.as_ref().map_or(
            (self.target_extent.0, self.target_extent.1, 0, (0, 0)),
            |source| (source.width, source.height, source.format, source.mip_layer),
        )
    }
}

impl Tracking {
    fn capture_state(&self, key: SurfaceKey, queue: usize) -> Option<D3d12CaptureSourceState> {
        let key = if self.resource_states.contains_key(&key) {
            key
        } else {
            (key.0, u32::MAX)
        };
        if self
            .resource_queues
            .get(&key)
            .is_none_or(|owner| *owner != queue)
        {
            return None;
        }
        self.resource_states.get(&key).and_then(|state| *state)
    }
    pub(crate) fn refresh_presentation(
        &self,
        presentation: &mut Presentation,
        queue: usize,
    ) -> bool {
        let Some(source) = presentation.source.as_mut() else {
            return true;
        };
        let Some(current) = source.at_subresource(source.subresource) else {
            return false;
        };
        if (current.width, current.height, current.format)
            != (source.width, source.height, source.format)
        {
            return false;
        }
        let key = Self::identity_key(&source.resource);
        source.state = self.capture_state((key, source.subresource), queue);
        source.sample_count == 1
            && source
                .state
                .is_some_and(|state| state.capture_supported(self.enhanced_barriers_supported()))
    }
}
