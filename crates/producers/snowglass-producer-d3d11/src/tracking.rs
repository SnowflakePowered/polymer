//! Draw-time render-region tracking for D3D11 swapchain backbuffers.
//!
//! The vtable interception that drives this lives in the D3D11 hook crate; this
//! module owns only the capture-region bookkeeping keyed by backbuffer.

use crate::D3d11CaptureRegion;
use dashmap::DashMap;
use std::{ffi::c_void, sync::LazyLock};
use windows::{
    Win32::Graphics::{
        Direct3D11::{
            D3D11_RASTERIZER_DESC, D3D11_VIEWPORT, ID3D11DeviceContext, ID3D11Resource,
            ID3D11Texture2D,
        },
        Dxgi::IDXGISwapChain,
    },
    core::{IUnknown, Interface},
};

#[derive(Clone, Copy)]
struct Candidate {
    region: D3d11CaptureRegion,
    area: u64,
}

static CANDIDATES: LazyLock<DashMap<usize, Candidate>> = LazyLock::new(Default::default);

/// Records the render region in effect for the render target bound to `context`
/// at draw time. Called by the D3D11 hook's draw trampolines.
///
/// # Safety
///
/// `context` must be null or point to a live `ID3D11DeviceContext`.
pub unsafe fn record_context_draw(context: *mut c_void) {
    let Some(context) = (unsafe { ID3D11DeviceContext::from_raw_borrowed(&context) }) else {
        return;
    };
    let mut targets = [None];
    unsafe { context.OMGetRenderTargets(Some(&mut targets), None) };
    let Some(target) = targets[0].as_ref() else {
        return;
    };
    let Ok(resource) = (unsafe { target.GetResource() }) else {
        return;
    };
    let Some(region) = current_region(context) else {
        return;
    };
    let area = u64::from(region.width) * u64::from(region.height);
    let key = identity_key(&resource);
    CANDIDATES
        .entry(key)
        .and_modify(|candidate| {
            if area > candidate.area {
                *candidate = Candidate { region, area };
            }
        })
        .or_insert(Candidate { region, area });
}

pub(crate) fn take_swapchain_region(swapchain: *mut c_void) -> Option<D3d11CaptureRegion> {
    snowglass_hook::config::tracking_enabled().then_some(())?;
    let swapchain = unsafe { IDXGISwapChain::from_raw_borrowed(&swapchain) }?;
    let texture: ID3D11Texture2D = unsafe { swapchain.GetBuffer(0).ok()? };
    let resource: ID3D11Resource = texture.cast().ok()?;
    CANDIDATES
        .remove(&identity_key(&resource))
        .map(|(_, value)| value.region)
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
