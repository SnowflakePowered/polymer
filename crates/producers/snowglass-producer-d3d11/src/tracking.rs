//! Presentation dependency tracking for D3D11 swapchain backbuffers.
//!
//! Draws establish edges from their render target to the 2D textures sampled
//! by the pixel shader. For ordinary presentation quads, the bound vertex data
//! also describes where that texture is placed and which UV subrectangle is
//! used. This lets capture follow the pre-composition image without inspecting
//! shader constants or reading rendered pixels.

use crate::D3d11CaptureRegion;
use dashmap::{DashMap, DashSet};
use std::{ffi::c_void, slice};
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

#[derive(Clone)]
struct Candidate {
    region: D3d11CaptureRegion,
    draw_area: u64,
    target_extent: (u32, u32),
    source: Option<TrackedResource>,
    exact: bool,
}

impl Candidate {
    fn rank(&self) -> (bool, u64, u64) {
        let source_area = self.source.as_ref().map_or(0, |source| {
            u64::from(source.width) * u64::from(source.height)
        });
        candidate_rank(self.exact, self.draw_area, source_area)
    }
}

fn candidate_rank(exact: bool, draw_area: u64, source_area: u64) -> (bool, u64, u64) {
    (exact, draw_area, source_area)
}

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
    pub sample_count: u32,
}

/// The presentation edge resolved for a swapchain backbuffer.
pub struct Presentation {
    pub region: D3d11CaptureRegion,
    pub source: Option<TrackedResource>,
    pub target_extent: (u32, u32),
}

/// Device-scoped state shared by the D3D11 hook contexts.
pub struct Tracking {
    /// Strongest observed composition edge keyed by destination texture.
    edges: DashMap<usize, Candidate>,
    /// Backbuffers that are graph sinks. Restricting dependency queries and
    /// tiny geometry readbacks to these targets keeps ordinary draws cheap.
    swapchain_targets: DashSet<usize>,
    graph_tracking: bool,
}

impl Default for Tracking {
    fn default() -> Self {
        Self {
            edges: DashMap::new(),
            swapchain_targets: DashSet::new(),
            graph_tracking: snowglass_hook::config::graph_tracking_enabled(),
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
        let mut targets = [None];
        unsafe { context.OMGetRenderTargets(Some(&mut targets), None) };
        let Some(target) = targets[0].as_ref() else {
            return;
        };
        let Ok(target_resource) = (unsafe { target.GetResource() }) else {
            return;
        };
        let Ok(target_texture) = target_resource.cast::<ID3D11Texture2D>() else {
            return;
        };
        let mut target_desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { target_texture.GetDesc(&mut target_desc) };
        let target_key = Self::identity_key(&target_resource);
        if self.graph_tracking && !self.swapchain_targets.contains(&target_key) {
            return;
        }
        let Some(mut region) = Self::current_region(context) else {
            return;
        };
        let mut source = self
            .graph_tracking
            .then(|| Self::dominant_sampled_texture(context, target_key))
            .flatten();
        let mut exact = false;
        if self.graph_tracking
            && let (Some((vertex_count, start_vertex)), Some(sampled)) = (vertices, source.as_mut())
            && let Some((quad_region, source_region)) =
                presentation_quad(context, vertex_count, start_vertex, region, sampled)
        {
            region = quad_region;
            if sampled.sample_count == 1 {
                sampled.x = source_region.x;
                sampled.y = source_region.y;
                sampled.width = source_region.width;
                sampled.height = source_region.height;
            }
            exact = true;
        }
        let observed = Candidate {
            region,
            draw_area: u64::from(region.width) * u64::from(region.height),
            target_extent: (target_desc.Width, target_desc.Height),
            source,
            exact,
        };
        self.edges
            .entry(target_key)
            .and_modify(|current| {
                if observed.rank() >= current.rank() {
                    *current = observed.clone();
                }
            })
            .or_insert(observed);
    }

    pub(crate) fn take_swapchain_presentation(
        &self,
        swapchain: *mut c_void,
    ) -> Option<Presentation> {
        snowglass_hook::config::tracking_enabled().then_some(())?;
        let swapchain = unsafe { IDXGISwapChain::from_raw_borrowed(&swapchain) }?;
        let texture: ID3D11Texture2D = unsafe { swapchain.GetBuffer(0).ok()? };
        let resource: ID3D11Resource = texture.cast().ok()?;
        self.swapchain_targets.insert(Self::identity_key(&resource));
        self.edges
            .remove(&Self::identity_key(&resource))
            .map(|(_, value)| Presentation {
                region: value.region,
                source: value.source,
                target_extent: value.target_extent,
            })
    }

    fn dominant_sampled_texture(
        context: &ID3D11DeviceContext,
        target_key: usize,
    ) -> Option<TrackedResource> {
        let mut views: [Option<ID3D11ShaderResourceView>; SHADER_RESOURCE_SLOTS] =
            std::array::from_fn(|_| None);
        unsafe { context.PSGetShaderResources(0, Some(&mut views)) };
        views
            .iter()
            .flatten()
            .filter_map(Self::tracked_resource)
            .filter(|source| {
                let Ok(resource) = source.texture.cast::<ID3D11Resource>() else {
                    return false;
                };
                Self::identity_key(&resource) != target_key
                    && source.width >= 32
                    && source.height >= 32
            })
            .max_by_key(|source| u64::from(source.width) * u64::from(source.height))
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
    raster_region: D3d11CaptureRegion,
    source: &TrackedResource,
) -> Option<(D3d11CaptureRegion, D3d11CaptureRegion)> {
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
    let quad_region = intersect_region(float_region(left, top, right, bottom)?, raster_region)?;
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
    Some((quad_region, source_region))
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
    fn composition_edges_beat_unsampled_draws_of_the_same_area() {
        assert!(candidate_rank(false, 1_000, 640 * 480) > candidate_rank(false, 1_000, 0));
    }

    #[test]
    fn larger_composition_sources_win_equal_area_draws() {
        assert!(
            candidate_rank(false, 1_000, 1920 * 1080) > candidate_rank(false, 1_000, 256 * 256)
        );
    }

    #[test]
    fn exact_quad_edges_beat_larger_raster_fallbacks() {
        assert!(candidate_rank(true, 1_000, 640 * 480) > candidate_rank(false, 2_000, 640 * 480));
    }

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
