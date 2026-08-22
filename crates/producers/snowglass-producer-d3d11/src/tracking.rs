//! Presentation dependency tracking for D3D11 swapchain backbuffers.
//!
//! Draws establish edges from their render target to the 2D textures sampled
//! by the graphics pipeline. At present time the edge reaching the swapchain
//! identifies the emulator's offscreen screen texture, allowing capture before
//! letterbox or pillarbox composition without inspecting shader constants or
//! reading rendered pixels.

use crate::D3d11CaptureRegion;
use dashmap::DashMap;
use std::ffi::c_void;
use windows::{
    Win32::Graphics::{
        Direct3D::{
            D3D_SRV_DIMENSION_TEXTURE2D, D3D_SRV_DIMENSION_TEXTURE2DARRAY,
            D3D_SRV_DIMENSION_TEXTURE2DMS, D3D_SRV_DIMENSION_TEXTURE2DMSARRAY,
        },
        Direct3D11::{
            D3D11_RASTERIZER_DESC, D3D11_SHADER_RESOURCE_VIEW_DESC, D3D11_TEXTURE2D_DESC,
            D3D11_VIEWPORT, ID3D11DeviceContext, ID3D11Resource, ID3D11ShaderResourceView,
            ID3D11Texture2D,
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
}

impl Candidate {
    fn rank(&self) -> (u64, u64) {
        let source_area = self.source.as_ref().map_or(0, |source| {
            u64::from(source.width) * u64::from(source.height)
        });
        candidate_rank(self.draw_area, source_area)
    }
}

fn candidate_rank(draw_area: u64, source_area: u64) -> (u64, u64) {
    (draw_area, source_area)
}

/// A 2D shader resource sampled by a presentation draw.
#[derive(Clone)]
pub struct TrackedResource {
    pub texture: ID3D11Texture2D,
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
#[derive(Default)]
pub struct Tracking {
    /// Strongest observed composition edge keyed by destination texture.
    edges: DashMap<usize, Candidate>,
}

impl Tracking {
    /// Records the render target and sampled-texture dependencies at draw time.
    ///
    /// # Safety
    ///
    /// `context` must be null or point to a live `ID3D11DeviceContext`.
    pub unsafe fn record_context_draw(&self, context: *mut c_void) {
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
        let Some(region) = Self::current_region(context) else {
            return;
        };
        let target_key = Self::identity_key(&target_resource);
        let source = Self::dominant_sampled_texture(context, target_key);
        let observed = Candidate {
            region,
            draw_area: u64::from(region.width) * u64::from(region.height),
            target_extent: (target_desc.Width, target_desc.Height),
            source,
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
        let mut best = None;
        let mut inspect = |views: &[Option<ID3D11ShaderResourceView>]| {
            for view in views.iter().flatten() {
                let Some(source) = Self::tracked_resource(view) else {
                    continue;
                };
                let Ok(resource) = source.texture.cast::<ID3D11Resource>() else {
                    continue;
                };
                if Self::identity_key(&resource) == target_key
                    || source.width < 32
                    || source.height < 32
                {
                    continue;
                }
                let area = u64::from(source.width) * u64::from(source.height);
                if best.as_ref().is_none_or(|current: &TrackedResource| {
                    area > u64::from(current.width) * u64::from(current.height)
                }) {
                    best = Some(source);
                }
            }
        };

        let mut views: [Option<ID3D11ShaderResourceView>; SHADER_RESOURCE_SLOTS] =
            std::array::from_fn(|_| None);
        unsafe { context.VSGetShaderResources(0, Some(&mut views)) };
        inspect(&views);
        views.fill(None);
        unsafe { context.HSGetShaderResources(0, Some(&mut views)) };
        inspect(&views);
        views.fill(None);
        unsafe { context.DSGetShaderResources(0, Some(&mut views)) };
        inspect(&views);
        views.fill(None);
        unsafe { context.GSGetShaderResources(0, Some(&mut views)) };
        inspect(&views);
        views.fill(None);
        unsafe { context.PSGetShaderResources(0, Some(&mut views)) };
        inspect(&views);
        best
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composition_edges_beat_unsampled_draws_of_the_same_area() {
        assert!(candidate_rank(1_000, 640 * 480) > candidate_rank(1_000, 0));
    }

    #[test]
    fn larger_composition_sources_win_equal_area_draws() {
        assert!(candidate_rank(1_000, 1920 * 1080) > candidate_rank(1_000, 256 * 256));
    }
}
