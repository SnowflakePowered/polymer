//! Draw-time render-region tracking for D3D11 swapchain backbuffers.
//!
//! The vtable interception that drives this lives in the D3D11 hook crate; this
//! module owns only the capture-region bookkeeping keyed by backbuffer.

use crate::D3d11CaptureRegion;
use dashmap::DashMap;
use std::{ffi::c_void, mem::size_of};
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

const MAX_PRESENTATION_CONSTANT_BUFFER_SIZE: usize = 4096;
const CONSTANT_BUFFER_SLOTS: usize = 14;

struct ConstantBuffer {
    bytes: Box<[u8]>,
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
    pub unsafe fn record_context_draw(&self, context: *mut c_void, element_count: Option<u32>) {
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
        let exact_region = element_count
            .is_some_and(|count| (3..=6).contains(&count))
            .then(|| self.constant_buffer_region(context, (target_desc.Width, target_desc.Height)))
            .flatten()
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
    /// quad. Some renderers use a full-backbuffer viewport and carry the actual
    /// centered content rectangle in shader constants, so viewport/scissor
    /// state alone cannot exclude letterbox or pillarbox bars.
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
        let byte_width = desc.ByteWidth as usize;
        if byte_width < size_of::<[f32; 4]>()
            || byte_width > MAX_PRESENTATION_CONSTANT_BUFFER_SIZE
            || desc.BindFlags & D3D11_BIND_CONSTANT_BUFFER.0 as u32 == 0
        {
            return;
        }
        let mut bytes = vec![0; byte_width].into_boxed_slice();
        unsafe {
            std::ptr::copy_nonoverlapping(source_data.cast::<u8>(), bytes.as_mut_ptr(), byte_width)
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
        let mut best = None;
        let mut inspect = |buffers: &[Option<ID3D11Buffer>]| {
            for buffer in buffers.iter().flatten() {
                let Ok(resource) = buffer.cast::<ID3D11Resource>() else {
                    continue;
                };
                let Some(constants) = self.constant_buffers.get(&Self::identity_key(&resource))
                else {
                    continue;
                };
                let Some(region) = infer_centered_content_region(&constants.bytes, target_extent)
                else {
                    continue;
                };
                let area = u64::from(region.width) * u64::from(region.height);
                if best.is_none_or(|(_, best_area)| area > best_area) {
                    best = Some((region, area));
                }
            }
        };
        let mut buffers: [Option<ID3D11Buffer>; CONSTANT_BUFFER_SLOTS] = Default::default();
        unsafe { context.VSGetConstantBuffers(0, Some(&mut buffers)) };
        inspect(&buffers);
        buffers.fill(None);
        unsafe { context.HSGetConstantBuffers(0, Some(&mut buffers)) };
        inspect(&buffers);
        buffers.fill(None);
        unsafe { context.DSGetConstantBuffers(0, Some(&mut buffers)) };
        inspect(&buffers);
        buffers.fill(None);
        unsafe { context.GSGetConstantBuffers(0, Some(&mut buffers)) };
        inspect(&buffers);
        buffers.fill(None);
        unsafe { context.PSGetConstantBuffers(0, Some(&mut buffers)) };
        inspect(&buffers);
        buffers.fill(None);
        unsafe { context.CSGetConstantBuffers(0, Some(&mut buffers)) };
        inspect(&buffers);
        best.map(|(region, _)| region)
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

fn infer_centered_content_region(
    bytes: &[u8],
    target_extent: (u32, u32),
) -> Option<D3d11CaptureRegion> {
    let mut best = None;
    for offset in (0..=bytes.len().saturating_sub(size_of::<[f32; 4]>())).step_by(4) {
        let mut values = [0.0; 4];
        for (index, value) in values.iter_mut().enumerate() {
            let start = offset + index * size_of::<f32>();
            let raw: [u8; 4] = bytes[start..start + 4].try_into().ok()?;
            *value = f32::from_ne_bytes(raw);
        }
        for edges in [
            values,
            [
                values[0],
                values[1],
                values[0] + values[2],
                values[1] + values[3],
            ],
        ] {
            let Some(region) = centered_bar_region(edges, target_extent) else {
                continue;
            };
            let area = u64::from(region.width) * u64::from(region.height);
            if best.is_none_or(|(_, best_area)| area > best_area) {
                best = Some((region, area));
            }
        }
    }
    best.map(|(region, _)| region)
}

fn centered_bar_region(edges: [f32; 4], target_extent: (u32, u32)) -> Option<D3d11CaptureRegion> {
    if target_extent.0 == 0
        || target_extent.1 == 0
        || edges.iter().any(|value| !value.is_finite())
        || edges
            .iter()
            .any(|value| (value.round() - value).abs() > 0.25)
    {
        return None;
    }
    let [left, top, right, bottom] = edges.map(|value| value.round() as i64);
    let target_width = i64::from(target_extent.0);
    let target_height = i64::from(target_extent.1);
    if left < 0
        || top < 0
        || right > target_width
        || bottom > target_height
        || right <= left
        || bottom <= top
    {
        return None;
    }
    let width = right - left;
    let height = bottom - top;
    let fills_width = left == 0 && right == target_width;
    let fills_height = top == 0 && bottom == target_height;
    let centered_pillarbox = fills_height
        && !fills_width
        && (left - (target_width - right)).abs() <= 1
        && width >= target_width / 4;
    let centered_letterbox = fills_width
        && !fills_height
        && (top - (target_height - bottom)).abs() <= 1
        && height >= target_height / 4;
    (centered_pillarbox || centered_letterbox).then_some(D3d11CaptureRegion {
        x: left as u32,
        y: top as u32,
        width: width as u32,
        height: height as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_f32(bytes: &mut [u8], offset: usize, value: f32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
    }

    fn constants_with(rect: [f32; 4], offset: usize) -> [u8; 128] {
        let mut bytes = [0; 128];
        for (index, value) in rect.into_iter().enumerate() {
            write_f32(&mut bytes, offset + index * 4, value);
        }
        bytes
    }

    #[test]
    fn infers_centered_pillarbox_rectangle_at_an_arbitrary_offset() {
        let constants = constants_with([160.0, 0.0, 1760.0, 900.0], 28);
        assert_eq!(
            infer_centered_content_region(&constants, (1920, 900)),
            Some(D3d11CaptureRegion {
                x: 160,
                y: 0,
                width: 1600,
                height: 900,
            })
        );
    }

    #[test]
    fn infers_centered_letterbox_rectangle_encoded_as_xywh() {
        let constants = constants_with([0.0, 90.0, 1920.0, 720.0], 44);
        assert_eq!(
            infer_centered_content_region(&constants, (1920, 900)),
            Some(D3d11CaptureRegion {
                x: 0,
                y: 90,
                width: 1920,
                height: 720,
            })
        );
    }

    #[test]
    fn rejects_off_center_and_full_surface_rectangles() {
        let off_center = constants_with([100.0, 0.0, 1700.0, 900.0], 12);
        let full_surface = constants_with([0.0, 0.0, 1920.0, 900.0], 64);
        assert_eq!(
            infer_centered_content_region(&off_center, (1920, 900)),
            None
        );
        assert_eq!(
            infer_centered_content_region(&full_surface, (1920, 900)),
            None
        );
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
