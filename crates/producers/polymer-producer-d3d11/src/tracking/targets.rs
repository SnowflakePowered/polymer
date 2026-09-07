use super::*;
use windows::Win32::Graphics::Direct3D11::*;

impl Tracking {
    pub(super) fn render_target_resources(view: &ID3D11RenderTargetView) -> Vec<TrackedResource> {
        let Ok(resource) = (unsafe { view.GetResource() }) else {
            return Vec::new();
        };
        let Ok(texture) = resource.cast::<ID3D11Texture2D>() else {
            return Vec::new();
        };
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        let mut view_desc = D3D11_RENDER_TARGET_VIEW_DESC::default();
        unsafe {
            texture.GetDesc(&mut desc);
            view.GetDesc(&mut view_desc);
        }
        let (mip, first, count) = match view_desc.ViewDimension {
            D3D11_RTV_DIMENSION_TEXTURE2D => {
                (unsafe { view_desc.Anonymous.Texture2D.MipSlice }, 0, 1)
            }
            D3D11_RTV_DIMENSION_TEXTURE2DARRAY => {
                let view = unsafe { view_desc.Anonymous.Texture2DArray };
                (view.MipSlice, view.FirstArraySlice, view.ArraySize)
            }
            D3D11_RTV_DIMENSION_TEXTURE2DMS => (0, 0, 1),
            D3D11_RTV_DIMENSION_TEXTURE2DMSARRAY => {
                let view = unsafe { view_desc.Anonymous.Texture2DMSArray };
                (0, view.FirstArraySlice, view.ArraySize)
            }
            _ => return Vec::new(),
        };
        if mip >= desc.MipLevels {
            return Vec::new();
        }
        (first..first.saturating_add(count).min(desc.ArraySize))
            .map(|layer| TrackedResource {
                texture: texture.clone(),
                x: 0,
                y: 0,
                width: desc.Width.checked_shr(mip).unwrap_or(0).max(1),
                height: desc.Height.checked_shr(mip).unwrap_or(0).max(1),
                format: desc.Format.0 as u32,
                subresource: mip + layer * desc.MipLevels,
                mip_layer: (mip, layer),
                sample_count: desc.SampleDesc.Count,
            })
            .collect()
    }

    /// Records a clear independently of bound draw targets, viewport and scissor.
    /// # Safety
    /// Pointers must be null or live context/render-target-view interfaces.
    pub unsafe fn record_clear(&self, context: *mut c_void, view: *mut c_void) {
        if polymer_presentation::observation::CaptureScope::active() {
            return;
        }
        let Some(context) = (unsafe { ID3D11DeviceContext::from_raw_borrowed(&context) }) else {
            return;
        };
        let Some(view) = (unsafe { ID3D11RenderTargetView::from_raw_borrowed(&view) }) else {
            return;
        };
        let _observations = self.observations.transaction();
        for target in Self::render_target_resources(view) {
            let Ok(resource) = target.texture.cast::<ID3D11Resource>() else {
                continue;
            };
            let id = (Self::identity_key(&resource), target.subresource);
            let mut edge = DependencyEdge::sampled(id, id, self.observations.generation());
            edge.evidence = Evidence::Clear;
            edge.coverage = Some(
                Extent2D {
                    width: target.width,
                    height: target.height,
                }
                .rect(),
            );
            self.observe(
                context,
                RecordedObservation {
                    edge,
                    source: target.clone(),
                    destination: target,
                },
            );
        }
    }
}
