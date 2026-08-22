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
            D3D11_BIND_CONSTANT_BUFFER, D3D11_BOX, D3D11_BUFFER_DESC, D3D11_RASTERIZER_DESC,
            D3D11_VIEWPORT, ID3D11Buffer, ID3D11DeviceContext, ID3D11Resource, ID3D11Texture2D,
        },
        Dxgi::IDXGISwapChain,
    },
    core::{IUnknown, Interface},
};

#[derive(Clone, Copy)]
struct Candidate {
    region: D3d11CaptureRegion,
    area: u64,
    target_extent: (u32, u32),
    exact: bool,
}

/// Draw placement associated with a presented backbuffer.
pub struct Presentation {
    pub region: D3d11CaptureRegion,
    pub target_extent: (u32, u32),
}

impl Candidate {
    fn rank(self) -> (bool, u64) {
        (self.exact, self.area)
    }
}

const DISPLAY_CONSTANT_BUFFER_SIZE: usize = 96;

struct ConstantBuffer {
    bytes: [u8; DISPLAY_CONSTANT_BUFFER_SIZE],
}

/// Device-scoped state shared by the D3D11 hook contexts.
#[derive(Default)]
pub struct Tracking {
    candidates: DashMap<usize, Candidate>,
    constant_buffers: DashMap<usize, ConstantBuffer>,
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
        let Some(raster_region) = Self::current_region(context) else {
            return;
        };
        let exact_region = self
            .constant_buffer_region(context, (target_desc.Width, target_desc.Height))
            .and_then(|region| intersect_regions(region, raster_region));
        let (region, exact) = exact_region.map_or((raster_region, false), |region| (region, true));
        let area = u64::from(region.width) * u64::from(region.height);
        let target_key = Self::identity_key(&target_resource);
        let observed = Candidate {
            region,
            area,
            target_extent: (target_desc.Width, target_desc.Height),
            exact,
        };
        self.candidates
            .entry(target_key)
            .and_modify(|current| {
                if observed.rank() > current.rank() {
                    *current = observed;
                }
            })
            .or_insert(observed);
    }

    /// Retains small constant-buffer uploads which may describe a presentation
    /// quad. PCSX2 keeps a full-backbuffer viewport and carries the actual game
    /// rectangle in its 96-byte display constants, so viewport/scissor state
    /// alone cannot exclude its pillarbox bars.
    ///
    /// # Safety
    ///
    /// `resource` must be null or point to a live `ID3D11Resource`. When
    /// non-null, `source_data` must remain readable for the duration of this
    /// call, as required by `ID3D11DeviceContext::UpdateSubresource`.
    pub unsafe fn record_update_subresource(
        &self,
        resource: *mut c_void,
        subresource: u32,
        destination_box: *const D3D11_BOX,
        source_data: *const c_void,
    ) {
        if resource.is_null()
            || subresource != 0
            || !destination_box.is_null()
            || source_data.is_null()
        {
            return;
        }
        let Some(resource) = (unsafe { ID3D11Resource::from_raw_borrowed(&resource) }) else {
            return;
        };
        let Ok(buffer) = resource.cast::<ID3D11Buffer>() else {
            return;
        };
        let mut desc = D3D11_BUFFER_DESC::default();
        unsafe { buffer.GetDesc(&mut desc) };
        if desc.ByteWidth as usize != DISPLAY_CONSTANT_BUFFER_SIZE
            || desc.BindFlags & D3D11_BIND_CONSTANT_BUFFER.0 as u32 == 0
        {
            return;
        }
        let mut bytes = [0; DISPLAY_CONSTANT_BUFFER_SIZE];
        unsafe {
            std::ptr::copy_nonoverlapping(
                source_data.cast::<u8>(),
                bytes.as_mut_ptr(),
                DISPLAY_CONSTANT_BUFFER_SIZE,
            )
        };
        self.constant_buffers
            .insert(Self::identity_key(resource), ConstantBuffer { bytes });
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

    fn constant_buffer_region(
        &self,
        context: &ID3D11DeviceContext,
        target_extent: (u32, u32),
    ) -> Option<D3d11CaptureRegion> {
        let mut buffers: [Option<ID3D11Buffer>; 1] = [None];
        unsafe { context.PSGetConstantBuffers(0, Some(&mut buffers)) };
        let buffer = buffers[0].as_ref()?;
        let resource: ID3D11Resource = buffer.cast().ok()?;
        let constants = self.constant_buffers.get(&Self::identity_key(&resource))?;
        display_constant_region(&constants.bytes, target_extent)
    }

    fn identity_key(resource: &ID3D11Resource) -> usize {
        resource
            .cast::<IUnknown>()
            .map_or(resource.as_raw() as usize, |identity| {
                identity.as_raw() as usize
            })
    }
}

fn intersect_regions(
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

fn display_constant_region(
    bytes: &[u8; DISPLAY_CONSTANT_BUFFER_SIZE],
    target_extent: (u32, u32),
) -> Option<D3d11CaptureRegion> {
    let read = |offset: usize| {
        let raw: [u8; 4] = bytes.get(offset..offset + 4)?.try_into().ok()?;
        let value = f32::from_ne_bytes(raw);
        value.is_finite().then_some(value)
    };
    // PCSX2's DisplayConstantBuffer stores TargetRect at +16, TargetSize at
    // +40, TargetResolution at +48, and its reciprocal at +56. Check all four
    // fields so an unrelated 96-byte constant buffer cannot be mistaken for a
    // presentation rectangle.
    let rect = [read(16)?, read(20)?, read(24)?, read(28)?];
    let size = [read(40)?, read(44)?];
    let resolution = [read(48)?, read(52)?];
    let reciprocal = [read(56)?, read(60)?];
    let expected_width = target_extent.0 as f32;
    let expected_height = target_extent.1 as f32;
    let close = |left: f32, right: f32, tolerance: f32| (left - right).abs() <= tolerance;
    if target_extent.0 == 0
        || target_extent.1 == 0
        || !close(resolution[0], expected_width, 0.5)
        || !close(resolution[1], expected_height, 0.5)
        || !close(reciprocal[0], expected_width.recip(), 0.000_01)
        || !close(reciprocal[1], expected_height.recip(), 0.000_01)
        || !close(size[0], rect[2] - rect[0], 0.25)
        || !close(size[1], rect[3] - rect[1], 0.25)
    {
        return None;
    }
    let left = rect[0].round() as i64;
    let top = rect[1].round() as i64;
    let right = rect[2].round() as i64;
    let bottom = rect[3].round() as i64;
    if left < 0
        || top < 0
        || right > i64::from(target_extent.0)
        || bottom > i64::from(target_extent.1)
        || right <= left
        || bottom <= top
    {
        return None;
    }
    Some(D3d11CaptureRegion {
        x: left as u32,
        y: top as u32,
        width: (right - left) as u32,
        height: (bottom - top) as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_f32(bytes: &mut [u8], offset: usize, value: f32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
    }

    fn display_constants(rect: [f32; 4], extent: (u32, u32)) -> [u8; 96] {
        let mut bytes = [0; 96];
        for (index, value) in rect.into_iter().enumerate() {
            write_f32(&mut bytes, 16 + index * 4, value);
        }
        write_f32(&mut bytes, 40, rect[2] - rect[0]);
        write_f32(&mut bytes, 44, rect[3] - rect[1]);
        write_f32(&mut bytes, 48, extent.0 as f32);
        write_f32(&mut bytes, 52, extent.1 as f32);
        write_f32(&mut bytes, 56, (extent.0 as f32).recip());
        write_f32(&mut bytes, 60, (extent.1 as f32).recip());
        bytes
    }

    #[test]
    fn pcsx2_display_constants_recover_a_pillarboxed_rectangle() {
        let constants = display_constants([160.0, 0.0, 1760.0, 900.0], (1920, 900));
        assert_eq!(
            display_constant_region(&constants, (1920, 900)),
            Some(D3d11CaptureRegion {
                x: 160,
                y: 0,
                width: 1600,
                height: 900,
            })
        );
    }

    #[test]
    fn unrelated_constant_data_is_not_a_presentation_rectangle() {
        let mut constants = display_constants([160.0, 0.0, 1760.0, 900.0], (1920, 900));
        write_f32(&mut constants, 56, 1.0);
        assert_eq!(display_constant_region(&constants, (1920, 900)), None);
    }

    #[test]
    fn exact_rectangle_candidates_beat_larger_viewports() {
        let exact = Candidate {
            region: D3d11CaptureRegion {
                x: 160,
                y: 0,
                width: 1600,
                height: 900,
            },
            area: 1600 * 900,
            target_extent: (1920, 900),
            exact: true,
        };
        let viewport = Candidate {
            region: D3d11CaptureRegion {
                x: 0,
                y: 0,
                width: 1920,
                height: 900,
            },
            area: 1920 * 900,
            target_extent: (1920, 900),
            exact: false,
        };
        assert!(exact.rank() > viewport.rank());
    }
}
