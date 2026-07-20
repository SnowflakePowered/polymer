//! Draw-time render-region tracking for D3D11 swapchain backbuffers.
//!
//! The vtable interception that drives this lives in the D3D11 hook crate; this
//! module owns only the capture-region bookkeeping keyed by backbuffer.

use crate::D3d11CaptureRegion;
use dashmap::DashMap;
use std::ffi::c_void;
use windows::{
    Win32::Graphics::{
        Direct3D11::{
            D3D11_COMMONSHADER_INPUT_RESOURCE_SLOT_COUNT, D3D11_RASTERIZER_DESC, D3D11_VIEWPORT,
            ID3D11DeviceContext, ID3D11Resource, ID3D11ShaderResourceView, ID3D11Texture2D,
        },
        Dxgi::IDXGISwapChain,
    },
    core::{IUnknown, Interface},
};

#[derive(Clone)]
struct Candidate {
    region: D3d11CaptureRegion,
    area: u64,
    target_extent: (u32, u32),
    source: Option<TrackedResource>,
}

impl Candidate {
    fn rank(&self) -> (u64, u64) {
        let source_pixels = self.source.as_ref().map_or(0, |source| {
            u64::from(source.width) * u64::from(source.height)
        });
        (self.area, source_pixels)
    }
}

/// Draw placement and sampled texture associated with a presented backbuffer.
pub struct Presentation {
    pub region: D3d11CaptureRegion,
    pub target_extent: (u32, u32),
    pub source: Option<TrackedResource>,
}

/// A texture sampled by the draw that presented into a swapchain backbuffer.
#[derive(Clone)]
pub struct TrackedResource {
    pub resource: ID3D11Texture2D,
    pub width: u32,
    pub height: u32,
    pub format: u32,
}

/// Device-scoped state shared by the D3D11 hook contexts.
#[derive(Default)]
pub struct Tracking {
    candidates: DashMap<usize, Candidate>,
}

impl Tracking {
    /// Records the render region in effect for the render target bound to
    /// `context` at draw time. Called by the D3D11 hook's draw trampolines.
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
        let mut target_desc = windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC::default();
        unsafe { target_texture.GetDesc(&mut target_desc) };
        let Some(region) = Self::current_region(context) else {
            return;
        };
        let area = u64::from(region.width) * u64::from(region.height);
        let target_key = Self::identity_key(&target_resource);
        let source = Self::sampled_source(context, target_key);
        let observed = Candidate {
            region,
            area,
            target_extent: (target_desc.Width, target_desc.Height),
            source,
        };
        self.candidates
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
        self.candidates
            .remove(&Self::identity_key(&resource))
            .map(|(_, value)| Presentation {
                region: value.region,
                target_extent: value.target_extent,
                source: value.source,
            })
    }

    fn sampled_source(context: &ID3D11DeviceContext, target_key: usize) -> Option<TrackedResource> {
        let mut views: [Option<ID3D11ShaderResourceView>;
            D3D11_COMMONSHADER_INPUT_RESOURCE_SLOT_COUNT as usize] = std::array::from_fn(|_| None);
        unsafe { context.PSGetShaderResources(0, Some(&mut views)) };
        views
            .into_iter()
            .flatten()
            .filter_map(|view| unsafe { view.GetResource().ok() })
            .filter(|resource| Self::identity_key(resource) != target_key)
            .filter_map(|resource| resource.cast::<ID3D11Texture2D>().ok())
            .filter_map(|resource| {
                let mut desc =
                    windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC::default();
                unsafe { resource.GetDesc(&mut desc) };
                (desc.Width >= 64 && desc.Height >= 64).then_some(TrackedResource {
                    resource,
                    width: desc.Width,
                    height: desc.Height,
                    format: desc.Format.0 as u32,
                })
            })
            .max_by_key(|source| u64::from(source.width) * u64::from(source.height))
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
