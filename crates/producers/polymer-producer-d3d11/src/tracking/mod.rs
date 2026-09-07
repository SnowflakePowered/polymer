//! Presentation dependency tracking for D3D11 swapchain backbuffers.
//!
//! Draws establish edges from their render target to the 2D textures sampled
//! by the pixel shader. For ordinary presentation quads, the bound vertex data
//! also describes where that texture is placed and which UV subrectangle is
//! used. This lets capture follow the pre-composition image without inspecting
//! shader constants or reading rendered pixels.

mod commands;
mod targets;
use crate::D3d11CaptureRegion;
use commands::{DeferredState, RecordedObservation};
use dashmap::{DashMap, DashSet};
use polymer_presentation::{
    DependencyEdge, Evidence, Extent2D, ObservationStore, PresentationSelector, Rect,
    RegionMapping, Target,
};
use std::{
    ffi::c_void,
    slice,
    sync::atomic::{AtomicU64, Ordering},
};
use windows::{
    Win32::Graphics::{
        Direct3D::{
            D3D_SRV_DIMENSION_TEXTURE2D, D3D_SRV_DIMENSION_TEXTURE2DARRAY,
            D3D_SRV_DIMENSION_TEXTURE2DMS, D3D_SRV_DIMENSION_TEXTURE2DMSARRAY,
        },
        Direct3D11::{
            D3D11_BOX, D3D11_BUFFER_DESC, D3D11_CPU_ACCESS_READ, D3D11_MAP_READ,
            D3D11_MAPPED_SUBRESOURCE, D3D11_RASTERIZER_DESC, D3D11_SHADER_RESOURCE_VIEW_DESC,
            D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, D3D11_VIEWPORT, ID3D11Buffer,
            ID3D11DeviceContext, ID3D11Resource, ID3D11ShaderResourceView, ID3D11Texture2D,
        },
        Dxgi::IDXGISwapChain,
    },
    core::{IUnknown, Interface},
};

const SHADER_RESOURCE_SLOTS: usize = 128;

/// A 2D shader resource sampled by a presentation draw.
#[derive(Clone)]
pub struct TrackedResource {
    pub texture: ID3D11Texture2D,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub subresource: u32,
    // Cached from the resource descriptor, independently of its mip count.
    mip_layer: (u32, u32),
    pub sample_count: u32,
}

/// The presentation edge resolved for a swapchain backbuffer.
#[derive(Clone)]
pub struct Presentation {
    pub source: Option<TrackedResource>,
    pub selection: polymer_presentation::SurfaceSelection,
}

/// Device-scoped state shared by the D3D11 hook contexts.
pub struct Tracking {
    pub(crate) registry: polymer_presentation::registry::PresentationRegistry,
    pub(crate) presentation_clock: AtomicU64,
    /// Strongest observed composition edge keyed by destination texture.
    observations: ObservationStore<(usize, u32)>,
    resources: DashMap<(usize, u32), TrackedResource>,
    observation_order: AtomicU64,
    /// Backbuffers that are graph sinks. Restricting dependency queries and
    /// tiny geometry readbacks to these targets keeps ordinary draws cheap.
    swapchain_targets: DashSet<usize>,
    deferred: DashMap<usize, DeferredState>,
    command_lists: DashMap<usize, Vec<RecordedObservation>>,
    graph_tracking: bool,
}

impl Default for Tracking {
    fn default() -> Self {
        Self {
            registry: Default::default(),
            presentation_clock: AtomicU64::new(0),
            observations: ObservationStore::default(),
            resources: DashMap::new(),
            observation_order: AtomicU64::new(0),
            swapchain_targets: DashSet::new(),
            deferred: DashMap::new(),
            command_lists: DashMap::new(),
            graph_tracking: polymer_hook::config::graph_tracking_enabled(),
        }
    }
}

impl Tracking {
    /// Registers the swapchain's current backbuffer as a graph sink.
    ///
    /// # Safety
    ///
    /// `swapchain` must be null or point to a live `IDXGISwapChain`.
    pub unsafe fn register_swapchain(&self, swapchain: *mut c_void) {
        let Some(swapchain) = (unsafe { IDXGISwapChain::from_raw_borrowed(&swapchain) }) else {
            return;
        };
        let Ok(texture) = (unsafe { swapchain.GetBuffer::<ID3D11Texture2D>(0) }) else {
            return;
        };
        let Ok(resource) = texture.cast::<ID3D11Resource>() else {
            return;
        };
        self.swapchain_targets.insert(Self::identity_key(&resource));
    }

    /// Records the render target and sampled-texture dependencies at draw time.
    ///
    /// # Safety
    ///
    /// `context` must be null or point to a live `ID3D11DeviceContext`.
    pub unsafe fn record_context_draw(&self, context: *mut c_void, vertices: Option<(u32, u32)>) {
        let Some(context) = (unsafe { ID3D11DeviceContext::from_raw_borrowed(&context) }) else {
            return;
        };
        if polymer_presentation::observation::CaptureScope::active() {
            return;
        }
        let _observations = self.observations.transaction();
        let deferred = unsafe { context.GetType() }
            != windows::Win32::Graphics::Direct3D11::D3D11_DEVICE_CONTEXT_IMMEDIATE;
        let cached = self
            .deferred
            .get(&(context.as_raw() as usize))
            .map(|state| state.bindings.clone());
        let mut targets: [Option<windows::Win32::Graphics::Direct3D11::ID3D11RenderTargetView>; 8] =
            std::array::from_fn(|_| None);
        if deferred {
            if let Some(state) = &cached {
                for (destination, source) in targets.iter_mut().zip(&state.targets) {
                    *destination = source.clone();
                }
            }
        } else {
            unsafe { context.OMGetRenderTargets(Some(&mut targets), None) };
        }
        for target in targets.iter().flatten() {
            let region = if deferred {
                cached.as_ref().and_then(|state| state.region())
            } else {
                Self::current_region(context)
            };
            let Some(region) = region else {
                continue;
            };
            let generation = self.observations.generation();
            for destination_resource in Self::render_target_resources(target) {
                let Ok(resource) = destination_resource.texture.cast::<ID3D11Resource>() else {
                    continue;
                };
                let target_key = Self::identity_key(&resource);
                let destination = (target_key, destination_resource.subresource);
                let mut views: [Option<ID3D11ShaderResourceView>; SHADER_RESOURCE_SLOTS] =
                    std::array::from_fn(|_| None);
                if deferred {
                    if let Some(state) = &cached {
                        for (destination, source) in views.iter_mut().zip(&state.views) {
                            *destination = source.clone();
                        }
                    }
                } else {
                    unsafe { context.PSGetShaderResources(0, Some(&mut views)) };
                }
                let mut observed = false;
                for mut sampled in views.iter().flatten().flat_map(Self::tracked_resources) {
                    let Ok(resource) = sampled.texture.cast::<ID3D11Resource>() else {
                        continue;
                    };
                    let source = (Self::identity_key(&resource), sampled.subresource);
                    if source == destination {
                        continue;
                    }
                    let mut edge = DependencyEdge::sampled(source, destination, generation);
                    edge.coverage = Some(region.into());
                    if !deferred
                        && self.swapchain_targets.contains(&target_key)
                        && let Some((count, start)) = vertices
                        && let Some((quad, crop, orientation)) =
                            presentation_quad(context, count, start, region, &sampled)
                    {
                        edge.evidence = Evidence::RecoveredQuad;
                        edge.mapping = Some(RegionMapping {
                            source: crop.into(),
                            destination: quad.into(),
                            flip_x: orientation.0,
                            flip_y: orientation.1,
                        });
                        edge.coverage = Some(region.into());
                    }
                    sampled.x = 0;
                    sampled.y = 0;
                    self.observe(
                        context,
                        RecordedObservation {
                            edge,
                            source: sampled,
                            destination: destination_resource.clone(),
                        },
                    );
                    observed = true;
                }
                if !observed {
                    let mut edge = DependencyEdge::sampled(destination, destination, generation);
                    edge.coverage = Some(region.into());
                    self.observe(
                        context,
                        RecordedObservation {
                            edge,
                            source: destination_resource.clone(),
                            destination: destination_resource,
                        },
                    );
                }
            }
        }
    }

    pub(crate) fn take_swapchain_presentations(
        &self,
        swapchain: *mut c_void,
        limit: usize,
    ) -> Vec<Presentation> {
        let Some(swapchain) = (unsafe { IDXGISwapChain::from_raw_borrowed(&swapchain) }) else {
            return Vec::new();
        };
        let Ok(texture) = (unsafe { swapchain.GetBuffer::<ID3D11Texture2D>(0) }) else {
            return Vec::new();
        };
        let Ok(resource) = texture.cast::<ID3D11Resource>() else {
            return Vec::new();
        };
        let presented = (Self::identity_key(&resource), 0);
        let _observations = self.observations.transaction();
        let mut graph = self.observations.present(self.graph_tracking);
        graph.resolve(presented, |id| {
            let source = self.resources.get(&id)?;
            let (mip, layer) = source.mip_layer;
            Some((source.extent(), ((id.0, 0), mip, layer)))
        });
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };
        graph.surfaces.insert(
            presented,
            Extent2D {
                width: desc.Width,
                height: desc.Height,
            },
        );
        let selection = PresentationSelector::select(&graph, presented, limit, self.graph_tracking);
        let result = selection
            .surfaces
            .into_iter()
            .map(|selected| {
                let source = (selected.surface != presented)
                    .then(|| {
                        self.resources
                            .get(&selected.surface)
                            .map(|source| source.value().clone())
                    })
                    .flatten();
                Presentation {
                    source,
                    selection: selected.selection,
                }
            })
            .collect();
        if !self.graph_tracking {
            self.resources.clear();
        }
        self.resources
            .retain(|id, _| graph.surfaces.contains_key(id));
        result
    }

    /// Observes a texture copy or resolve after the application command.
    /// # Safety
    /// Resource pointers and an optional source box must be live for this call.
    pub unsafe fn record_copy(
        &self,
        context: *mut c_void,
        destination: *mut c_void,
        destination_subresource: u32,
        x: u32,
        y: u32,
        source: *mut c_void,
        source_subresource: u32,
        source_box: *const D3D11_BOX,
        resolve: bool,
    ) {
        if polymer_presentation::observation::CaptureScope::active() {
            return;
        }
        let _observations = self.observations.transaction();
        let Some(context) = (unsafe { ID3D11DeviceContext::from_raw_borrowed(&context) }) else {
            return;
        };
        let Some(source) = (unsafe { TrackedResource::from_raw(source, source_subresource) })
        else {
            return;
        };
        let Some(destination) =
            (unsafe { TrackedResource::from_raw(destination, destination_subresource) })
        else {
            return;
        };
        let source_key = source.key();
        let destination_key = destination.key();
        if source_key == destination_key {
            return;
        }
        let mut crop = source.extent().rect();
        if !source_box.is_null() {
            let bounds = unsafe { *source_box };
            let Some(width) = bounds.right.checked_sub(bounds.left) else {
                return;
            };
            let Some(height) = bounds.bottom.checked_sub(bounds.top) else {
                return;
            };
            crop = Rect {
                x: bounds.left as i32,
                y: bounds.top as i32,
                extent: Extent2D { width, height },
            };
        }
        let placement = Rect {
            x: x as i32,
            y: y as i32,
            extent: crop.extent,
        };
        if crop.intersect(source.extent().rect()) != Some(crop)
            || placement.intersect(destination.extent().rect()) != Some(placement)
        {
            return;
        }
        let generation = self.observations.generation();
        let mut edge = DependencyEdge::sampled(source_key, destination_key, generation);
        edge.evidence = if resolve {
            Evidence::Resolve
        } else {
            Evidence::Copy
        };
        edge.mapping = Some(RegionMapping {
            source: crop,
            destination: placement,
            flip_x: false,
            flip_y: false,
        });
        edge.coverage = Some(placement);
        self.observe(
            context,
            RecordedObservation {
                edge,
                source,
                destination,
            },
        );
    }

    /// Release only observations and native references belonging to the resized
    /// swapchain. Other swapchains and deferred-context bindings remain live.
    pub(crate) unsafe fn release_swapchain(&self, raw: *mut c_void) {
        let Some(swapchain) = (unsafe { IDXGISwapChain::from_raw_borrowed(&raw) }) else {
            return;
        };
        let Ok(desc) = (unsafe { swapchain.GetDesc() }) else {
            return;
        };
        let resources = (0..desc.BufferCount)
            .filter_map(|index| {
                unsafe { swapchain.GetBuffer::<ID3D11Resource>(index) }
                    .ok()
                    .map(|resource| Self::identity_key(&resource))
            })
            .collect::<std::collections::HashSet<_>>();
        self.forget_resources(&resources);
    }

    fn tracked_resources(view: &ID3D11ShaderResourceView) -> Vec<TrackedResource> {
        let Some(first) = Self::tracked_resource(view) else {
            return Vec::new();
        };
        let mut view_desc = D3D11_SHADER_RESOURCE_VIEW_DESC::default();
        let mut texture_desc = D3D11_TEXTURE2D_DESC::default();
        unsafe {
            view.GetDesc(&mut view_desc);
            first.texture.GetDesc(&mut texture_desc);
        }
        let count = match view_desc.ViewDimension {
            D3D_SRV_DIMENSION_TEXTURE2DARRAY => unsafe {
                view_desc.Anonymous.Texture2DArray.ArraySize
            },
            D3D_SRV_DIMENSION_TEXTURE2DMSARRAY => unsafe {
                view_desc.Anonymous.Texture2DMSArray.ArraySize
            },
            _ => 1,
        };
        if texture_desc.MipLevels == 0 {
            return Vec::new();
        }
        let remaining = texture_desc
            .ArraySize
            .saturating_sub(first.subresource / texture_desc.MipLevels);
        (0..count.min(remaining))
            .map(|layer| {
                let mut source = first.clone();
                source.subresource += layer * texture_desc.MipLevels;
                source
            })
            .collect()
    }

    fn tracked_resource(view: &ID3D11ShaderResourceView) -> Option<TrackedResource> {
        let resource = unsafe { view.GetResource().ok()? };
        let texture = resource.cast::<ID3D11Texture2D>().ok()?;
        let mut texture_desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut texture_desc) };
        let mut view_desc = D3D11_SHADER_RESOURCE_VIEW_DESC::default();
        unsafe { view.GetDesc(&mut view_desc) };

        let (mip, array_slice) = if view_desc.ViewDimension == D3D_SRV_DIMENSION_TEXTURE2D {
            (unsafe { view_desc.Anonymous.Texture2D.MostDetailedMip }, 0)
        } else if view_desc.ViewDimension == D3D_SRV_DIMENSION_TEXTURE2DARRAY {
            let desc = unsafe { view_desc.Anonymous.Texture2DArray };
            (desc.MostDetailedMip, desc.FirstArraySlice)
        } else if view_desc.ViewDimension == D3D_SRV_DIMENSION_TEXTURE2DMS {
            (0, 0)
        } else if view_desc.ViewDimension == D3D_SRV_DIMENSION_TEXTURE2DMSARRAY {
            let desc = unsafe { view_desc.Anonymous.Texture2DMSArray };
            (0, desc.FirstArraySlice)
        } else {
            return None;
        };
        if mip >= texture_desc.MipLevels || array_slice >= texture_desc.ArraySize {
            return None;
        }
        let width = (texture_desc.Width >> mip).max(1);
        let height = (texture_desc.Height >> mip).max(1);
        Some(TrackedResource {
            texture,
            x: 0,
            y: 0,
            width,
            height,
            format: texture_desc.Format.0 as u32,
            subresource: mip + array_slice * texture_desc.MipLevels,
            mip_layer: (mip, array_slice),
            sample_count: texture_desc.SampleDesc.Count,
        })
    }

    fn current_region(context: &ID3D11DeviceContext) -> Option<D3d11CaptureRegion> {
        let mut viewport_count = 1;
        let mut viewport = D3D11_VIEWPORT::default();
        unsafe { context.RSGetViewports(&mut viewport_count, Some(&mut viewport)) };
        if viewport_count == 0 {
            return None;
        }
        let mut left = viewport.TopLeftX.round() as i64;
        let mut top = viewport.TopLeftY.round() as i64;
        let mut right = (viewport.TopLeftX + viewport.Width).round() as i64;
        let mut bottom = (viewport.TopLeftY + viewport.Height).round() as i64;

        if let Ok(rasterizer) = unsafe { context.RSGetState() } {
            let mut desc = D3D11_RASTERIZER_DESC::default();
            unsafe { rasterizer.GetDesc(&mut desc) };
            if desc.ScissorEnable.as_bool() {
                let mut count = 1;
                let mut scissor = windows::Win32::Foundation::RECT::default();
                unsafe { context.RSGetScissorRects(&mut count, Some(&mut scissor)) };
                if count != 0 {
                    left = left.max(i64::from(scissor.left));
                    top = top.max(i64::from(scissor.top));
                    right = right.min(i64::from(scissor.right));
                    bottom = bottom.min(i64::from(scissor.bottom));
                }
            }
        }
        left = left.max(0);
        top = top.max(0);
        (right > left && bottom > top).then_some(D3d11CaptureRegion {
            x: left as u32,
            y: top as u32,
            width: (right - left) as u32,
            height: (bottom - top) as u32,
        })
    }

    fn identity_key(resource: &ID3D11Resource) -> usize {
        resource
            .cast::<IUnknown>()
            .map_or(resource.as_raw() as usize, |identity| {
                identity.as_raw() as usize
            })
    }
}

fn presentation_quad(
    context: &ID3D11DeviceContext,
    vertex_count: u32,
    start_vertex: u32,
    _raster_region: D3d11CaptureRegion,
    source: &TrackedResource,
) -> Option<(D3d11CaptureRegion, D3d11CaptureRegion, (bool, bool))> {
    if !(4..=6).contains(&vertex_count) {
        return None;
    }
    let mut buffer = None;
    let mut stride = 0;
    let mut offset = 0;
    unsafe {
        context.IAGetVertexBuffers(
            0,
            1,
            Some(&mut buffer),
            Some(&mut stride),
            Some(&mut offset),
        )
    };
    let buffer = buffer?;
    if stride < 8 || stride > 512 {
        return None;
    }
    let base =
        (offset as usize).checked_add((start_vertex as usize).checked_mul(stride as usize)?)?;
    let end = base.checked_add((vertex_count as usize).checked_mul(stride as usize)?)?;
    let vertices = read_buffer_range(context, &buffer, base, end)?;
    let position = find_position_field(&vertices, stride as usize, vertex_count as usize)?;
    let uv = find_uv_field(
        &vertices,
        stride as usize,
        vertex_count as usize,
        position.offset,
    )?;

    let mut viewport_count = 1;
    let mut viewport = D3D11_VIEWPORT::default();
    unsafe { context.RSGetViewports(&mut viewport_count, Some(&mut viewport)) };
    if viewport_count == 0 || viewport.Width <= 0.0 || viewport.Height <= 0.0 {
        return None;
    }
    let left = viewport.TopLeftX + (position.min_x + 1.0) * 0.5 * viewport.Width;
    let right = viewport.TopLeftX + (position.max_x + 1.0) * 0.5 * viewport.Width;
    let top = viewport.TopLeftY + (1.0 - position.max_y) * 0.5 * viewport.Height;
    let bottom = viewport.TopLeftY + (1.0 - position.min_y) * 0.5 * viewport.Height;
    let quad_region = float_region(left, top, right, bottom)?;
    let source_region = float_region(
        uv.min_x * source.width as f32,
        uv.min_y * source.height as f32,
        uv.max_x * source.width as f32,
        uv.max_y * source.height as f32,
    )?;
    let source_region = intersect_region(
        source_region,
        D3d11CaptureRegion {
            x: 0,
            y: 0,
            width: source.width,
            height: source.height,
        },
    )?;
    let orientation =
        position.orientation(uv, &vertices, stride as usize, vertex_count as usize)?;
    Some((quad_region, source_region, orientation))
}

fn read_buffer_range(
    context: &ID3D11DeviceContext,
    source: &ID3D11Buffer,
    start: usize,
    end: usize,
) -> Option<Vec<u8>> {
    let length = end.checked_sub(start)?;
    if length == 0 || end > u32::MAX as usize {
        return None;
    }
    let mut source_desc = D3D11_BUFFER_DESC::default();
    unsafe { source.GetDesc(&mut source_desc) };
    if end > source_desc.ByteWidth as usize {
        return None;
    }
    let device = unsafe { context.GetDevice().ok()? };
    let staging_desc = D3D11_BUFFER_DESC {
        ByteWidth: length as u32,
        Usage: D3D11_USAGE_STAGING,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        ..Default::default()
    };
    let mut staging = None;
    unsafe { device.CreateBuffer(&raw const staging_desc, None, Some(&mut staging)) }.ok()?;
    let staging = staging?;
    let source_box = D3D11_BOX {
        left: start as u32,
        top: 0,
        front: 0,
        right: end as u32,
        bottom: 1,
        back: 1,
    };
    unsafe {
        context.CopySubresourceRegion(&staging, 0, 0, 0, 0, source, 0, Some(&raw const source_box))
    };
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe { context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }.ok()?;
    let bytes = unsafe { slice::from_raw_parts(mapped.pData.cast::<u8>(), length) }.to_vec();
    unsafe { context.Unmap(&staging, 0) };
    Some(bytes)
}

#[derive(Clone, Copy)]
struct FloatBounds {
    offset: usize,
    min_x: f32,
    min_y: f32,
    max_x: f32,
    max_y: f32,
}

impl FloatBounds {
    fn orientation(
        self,
        uv: Self,
        vertices: &[u8],
        stride: usize,
        count: usize,
    ) -> Option<(bool, bool)> {
        let mut orientation = None;
        for vertex in vertices.chunks_exact(stride).take(count) {
            let read = |offset| {
                Some(f32::from_le_bytes(
                    vertex.get(offset..offset + 4)?.try_into().ok()?,
                ))
            };
            let (x, y, u, v) = (
                read(self.offset)?,
                read(self.offset + 4)?,
                read(uv.offset)?,
                read(uv.offset + 4)?,
            );
            let direction = (
                (x > (self.min_x + self.max_x) * 0.5) != (u > (uv.min_x + uv.max_x) * 0.5),
                (y < (self.min_y + self.max_y) * 0.5) != (v > (uv.min_y + uv.max_y) * 0.5),
            );
            if orientation.is_some_and(|old| old != direction) {
                return None;
            }
            orientation = Some(direction);
        }
        orientation
    }

    fn area(self) -> f32 {
        (self.max_x - self.min_x) * (self.max_y - self.min_y)
    }
}

fn field_bounds(
    vertices: &[u8],
    stride: usize,
    count: usize,
    field_offset: usize,
) -> Option<FloatBounds> {
    let mut points = Vec::with_capacity(count);
    for vertex in vertices.chunks_exact(stride).take(count) {
        let x = f32::from_le_bytes(
            vertex
                .get(field_offset..field_offset + 4)?
                .try_into()
                .ok()?,
        );
        let y = f32::from_le_bytes(
            vertex
                .get(field_offset + 4..field_offset + 8)?
                .try_into()
                .ok()?,
        );
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        points.push((x, y));
    }
    let min_x = points
        .iter()
        .map(|point| point.0)
        .fold(f32::INFINITY, f32::min);
    let min_y = points
        .iter()
        .map(|point| point.1)
        .fold(f32::INFINITY, f32::min);
    let max_x = points
        .iter()
        .map(|point| point.0)
        .fold(f32::NEG_INFINITY, f32::max);
    let max_y = points
        .iter()
        .map(|point| point.1)
        .fold(f32::NEG_INFINITY, f32::max);
    let epsilon = 0.0025;
    let corners = points.iter().fold(0u8, |mask, &(x, y)| {
        let x_side = if (x - min_x).abs() <= epsilon {
            0
        } else if (x - max_x).abs() <= epsilon {
            1
        } else {
            return mask;
        };
        let y_side = if (y - min_y).abs() <= epsilon {
            0
        } else if (y - max_y).abs() <= epsilon {
            1
        } else {
            return mask;
        };
        mask | 1 << (x_side + y_side * 2)
    });
    (corners == 0b1111).then_some(FloatBounds {
        offset: field_offset,
        min_x,
        min_y,
        max_x,
        max_y,
    })
}

fn find_position_field(vertices: &[u8], stride: usize, count: usize) -> Option<FloatBounds> {
    (0..=stride.saturating_sub(8))
        .step_by(4)
        .filter_map(|offset| field_bounds(vertices, stride, count, offset))
        .filter(|bounds| {
            bounds.min_x >= -1.25
                && bounds.max_x <= 1.25
                && bounds.min_y >= -1.25
                && bounds.max_y <= 1.25
                && bounds.min_x < -0.01
                && bounds.max_x > 0.01
                && bounds.min_y < -0.01
                && bounds.max_y > 0.01
                && bounds.area() > 0.01
        })
        .max_by(|left, right| left.area().total_cmp(&right.area()))
}

fn find_uv_field(
    vertices: &[u8],
    stride: usize,
    count: usize,
    position_offset: usize,
) -> Option<FloatBounds> {
    (0..=stride.saturating_sub(8))
        .step_by(4)
        .filter(|&offset| offset != position_offset)
        .filter_map(|offset| field_bounds(vertices, stride, count, offset))
        .filter(|bounds| {
            bounds.min_x >= -0.01
                && bounds.max_x <= 1.01
                && bounds.min_y >= -0.01
                && bounds.max_y <= 1.01
                && bounds.max_x - bounds.min_x > 0.01
                && bounds.max_y - bounds.min_y > 0.01
        })
        .max_by(|left, right| left.area().total_cmp(&right.area()))
}

fn float_region(left: f32, top: f32, right: f32, bottom: f32) -> Option<D3d11CaptureRegion> {
    let left = left.floor().max(0.0) as u32;
    let top = top.floor().max(0.0) as u32;
    let right = right.ceil().max(0.0) as u32;
    let bottom = bottom.ceil().max(0.0) as u32;
    (right > left && bottom > top).then_some(D3d11CaptureRegion {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

fn intersect_region(
    left: D3d11CaptureRegion,
    right: D3d11CaptureRegion,
) -> Option<D3d11CaptureRegion> {
    let x = left.x.max(right.x);
    let y = left.y.max(right.y);
    let right_edge = left
        .x
        .saturating_add(left.width)
        .min(right.x.saturating_add(right.width));
    let bottom_edge = left
        .y
        .saturating_add(left.height)
        .min(right.y.saturating_add(right.height));
    (right_edge > x && bottom_edge > y).then_some(D3d11CaptureRegion {
        x,
        y,
        width: right_edge - x,
        height: bottom_edge - y,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_position_and_uv_fields_in_an_interleaved_quad() {
        let vertices: [[f32; 8]; 4] = [
            [-0.75, 1.0, 0.5, 1.0, 0.125, 0.0, 0.0, 0.0],
            [0.75, 1.0, 0.5, 1.0, 0.875, 0.0, 0.0, 0.0],
            [-0.75, -1.0, 0.5, 1.0, 0.125, 1.0, 0.0, 0.0],
            [0.75, -1.0, 0.5, 1.0, 0.875, 1.0, 0.0, 0.0],
        ];
        let bytes = unsafe {
            slice::from_raw_parts(vertices.as_ptr().cast::<u8>(), size_of_val(&vertices))
        };
        let position = find_position_field(bytes, 32, 4).unwrap();
        let uv = find_uv_field(bytes, 32, 4, position.offset).unwrap();
        assert_eq!(position.offset, 0);
        assert_eq!(uv.offset, 16);
        assert_eq!((uv.min_x, uv.max_x), (0.125, 0.875));
    }
}

impl TrackedResource {
    fn key(&self) -> (usize, u32) {
        (
            self.texture
                .cast::<IUnknown>()
                .map_or(self.texture.as_raw() as usize, |resource| {
                    resource.as_raw() as usize
                }),
            self.subresource,
        )
    }

    pub(crate) unsafe fn from_raw(raw: *mut c_void, subresource: u32) -> Option<Self> {
        let resource = unsafe { ID3D11Resource::from_raw_borrowed(&raw) }?;
        let texture = resource.cast::<ID3D11Texture2D>().ok()?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };
        if desc.MipLevels == 0 || subresource >= desc.MipLevels.checked_mul(desc.ArraySize)? {
            return None;
        }
        let mip = subresource % desc.MipLevels;
        Some(Self {
            texture,
            x: 0,
            y: 0,
            width: desc.Width.checked_shr(mip).unwrap_or(0).max(1),
            height: desc.Height.checked_shr(mip).unwrap_or(0).max(1),
            format: desc.Format.0 as u32,
            subresource,
            mip_layer: (mip, subresource / desc.MipLevels),
            sample_count: desc.SampleDesc.Count,
        })
    }
}

impl From<D3d11CaptureRegion> for Rect {
    fn from(region: D3d11CaptureRegion) -> Self {
        Self {
            x: region.x as i32,
            y: region.y as i32,
            extent: Extent2D {
                width: region.width,
                height: region.height,
            },
        }
    }
}
impl From<Rect> for D3d11CaptureRegion {
    fn from(rect: Rect) -> Self {
        Self {
            x: rect.x.max(0) as u32,
            y: rect.y.max(0) as u32,
            width: rect.extent.width,
            height: rect.extent.height,
        }
    }
}

impl polymer_presentation::history::HistorySurface for Presentation {
    fn selection(&mut self) -> &mut polymer_presentation::SurfaceSelection {
        &mut self.selection
    }
    type Id = (usize, u32);
    type Layout = (u32, u32, u32, (u32, u32));
    fn identity(&self) -> Self::Id {
        self.source.as_ref().map_or((0, 0), |source| {
            (source.texture.as_raw() as usize, source.subresource)
        })
    }
    fn layout(&self) -> Self::Layout {
        self.source.as_ref().map_or(
            (
                self.selection.source_rect.extent.width,
                self.selection.source_rect.extent.height,
                0,
                (0, 0),
            ),
            |source| {
                (
                    self.selection.source_rect.extent.width,
                    self.selection.source_rect.extent.height,
                    source.format,
                    source.mip_layer,
                )
            },
        )
    }
}

impl Presentation {
    pub(crate) fn revalidate(&mut self) -> bool {
        let Some(source) = &self.source else {
            return true;
        };
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { source.texture.GetDesc(&mut desc) };
        let levels = desc.MipLevels.max(1);
        let mip = source.subresource % levels;
        let extent = Extent2D {
            width: (desc.Width >> mip).max(1),
            height: (desc.Height >> mip).max(1),
        };
        source.subresource < levels.saturating_mul(desc.ArraySize)
            && source.format == desc.Format.0 as u32
            && self.selection.source_rect.intersect(extent.rect())
                == Some(self.selection.source_rect)
    }
}

impl TrackedResource {
    fn extent(&self) -> Extent2D {
        Extent2D {
            width: self.width,
            height: self.height,
        }
    }
}
