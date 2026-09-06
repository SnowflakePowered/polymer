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
            let mut edge = DependencyEdge::sampled(id, id, self.generation.load(Ordering::Relaxed));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deferred_clear_preserves_view_subresource_and_waits_for_execution() {
        unsafe {
            let mut device = None;
            D3D11CreateDevice(
                None,
                windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_WARP,
                windows::Win32::Foundation::HMODULE::default(),
                D3D11_CREATE_DEVICE_FLAG(0),
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
            .unwrap();
            let device = device.unwrap();
            let mut context = None;
            device.CreateDeferredContext(0, Some(&mut context)).unwrap();
            let context = context.unwrap();
            let mut texture = None;
            device
                .CreateTexture2D(
                    &D3D11_TEXTURE2D_DESC {
                        Width: 64,
                        Height: 48,
                        MipLevels: 2,
                        ArraySize: 2,
                        Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R8G8B8A8_UNORM,
                        SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
                            Count: 1,
                            Quality: 0,
                        },
                        Usage: D3D11_USAGE_DEFAULT,
                        BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                        ..Default::default()
                    },
                    None,
                    Some(&mut texture),
                )
                .unwrap();
            let mut view = None;
            device
                .CreateRenderTargetView(
                    &texture.unwrap(),
                    Some(&D3D11_RENDER_TARGET_VIEW_DESC {
                        ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2DARRAY,
                        Anonymous: D3D11_RENDER_TARGET_VIEW_DESC_0 {
                            Texture2DArray: D3D11_TEX2D_ARRAY_RTV {
                                MipSlice: 1,
                                FirstArraySlice: 1,
                                ArraySize: 1,
                            },
                        },
                        ..Default::default()
                    }),
                    Some(&mut view),
                )
                .unwrap();
            let view = view.unwrap();
            let sources = Tracking::render_target_resources(&view);
            assert_eq!(sources[0].mip_layer, (1, 1));
            let resource = sources[0].texture.cast::<ID3D11Resource>().unwrap();
            for (subresource, expected) in [(1, (1, 0)), (2, (0, 1)), (3, (1, 1))] {
                let source = TrackedResource::from_raw(resource.as_raw(), subresource).unwrap();
                assert_eq!(source.mip_layer, expected);
            }
            let tracking = Tracking::default();
            tracking.deferred_context_created(context.as_raw());
            context.ClearRenderTargetView(&view, &[1.0, 0.0, 0.0, 1.0]);
            tracking.record_clear(context.as_raw(), view.as_raw());
            let mut list = None;
            context.FinishCommandList(false, Some(&mut list)).unwrap();
            let list = list.unwrap();
            tracking.finish_commands(context.as_raw(), list.as_raw(), false);
            assert!(tracking.observations.snapshot(0).0.is_empty());
            tracking.execute_commands(list.as_raw(), || {});
            let (targets, edges) = tracking.observations.snapshot(0);
            assert_eq!(targets.len(), 1);
            assert_eq!(targets[0].id.1, 3);
            assert_eq!(targets[0].score, 0);
            assert_eq!(edges[0].coverage.unwrap().extent.area(), 32 * 24);
            assert_eq!(edges[0].source, edges[0].destination);
            tracking.execute_commands(list.as_raw(), || {});
            let (_, replay) = tracking.observations.snapshot(0);
            assert_eq!(replay.len(), 1);
            assert!(replay[0].order > edges[0].order);
            tracking.observations.clear();
            tracking.forget_commands(list.as_raw() as usize);
            tracking.execute_commands(list.as_raw(), || {});
            assert!(tracking.observations.snapshot(0).0.is_empty());
            // Finish transferred the recorded clear into the immutable batch.
            // A second list without another clear must not replay that write.
            let mut empty = None;
            context.FinishCommandList(false, Some(&mut empty)).unwrap();
            let empty = empty.unwrap();
            tracking.finish_commands(context.as_raw(), empty.as_raw(), false);
            tracking.execute_commands(empty.as_raw(), || {});
            assert!(tracking.observations.snapshot(0).0.is_empty());
        }
    }
}
