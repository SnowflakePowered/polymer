use super::*;
use polymer_presentation::RegionMapping;

impl Tracking {
    /// # Safety
    /// Locations and optional box follow the native CopyTextureRegion contract.
    pub unsafe fn copy_texture(
        &self,
        list: *mut c_void,
        destination: *const windows::Win32::Graphics::Direct3D12::D3D12_TEXTURE_COPY_LOCATION,
        x: u32,
        y: u32,
        source: *const windows::Win32::Graphics::Direct3D12::D3D12_TEXTURE_COPY_LOCATION,
        source_box: *const windows::Win32::Graphics::Direct3D12::D3D12_BOX,
    ) {
        use windows::Win32::Graphics::Direct3D12::D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX;
        if polymer_presentation::observation::CaptureScope::active() {
            return;
        }
        let (Some(destination), Some(source)) =
            (unsafe { destination.as_ref() }, unsafe { source.as_ref() })
        else {
            return;
        };
        if destination.Type != D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX
            || source.Type != D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX
        {
            return;
        }
        let destination_subresource = unsafe { destination.Anonymous.SubresourceIndex };
        let source_subresource = unsafe { source.Anonymous.SubresourceIndex };
        let (Some(destination), Some(source)) =
            (destination.pResource.as_ref(), source.pResource.as_ref())
        else {
            return;
        };
        let Some(source) = (unsafe { Self::tracked_resource(source.as_raw()) })
            .and_then(|source| source.at_subresource(source_subresource))
        else {
            return;
        };
        let Some(target) = (unsafe { Self::tracked_resource(destination.as_raw()) })
            .and_then(|target| target.at_subresource(destination_subresource))
        else {
            return;
        };
        let mut crop = source.extent().rect();
        if let Some(bounds) = unsafe { source_box.as_ref() } {
            let (Some(width), Some(height)) = (
                bounds.right.checked_sub(bounds.left),
                bounds.bottom.checked_sub(bounds.top),
            ) else {
                return;
            };
            crop = polymer_presentation::Rect {
                x: bounds.left as i32,
                y: bounds.top as i32,
                extent: Extent2D { width, height },
            };
        }
        let target_extent = Extent2D {
            width: target.width,
            height: target.height,
        };
        let placement = polymer_presentation::Rect {
            x: x as i32,
            y: y as i32,
            extent: crop.extent,
        };
        if crop.intersect(source.extent().rect()) != Some(crop)
            || placement.intersect(target_extent.rect()) != Some(placement)
        {
            return;
        }
        self.commands
            .entry(list as usize)
            .or_default()
            .observations
            .push((
                (Self::identity_key(destination), destination_subresource),
                Candidate {
                    evidence: polymer_presentation::Evidence::Copy,
                    region: D3d12CaptureRegion {
                        x,
                        y,
                        width: crop.extent.width,
                        height: crop.extent.height,
                    },
                    draw_area: crop.extent.area(),
                    source: Some(source),
                    bindings: None,
                    mapping: Some(RegionMapping {
                        source: crop,
                        destination: placement,
                        flip_x: false,
                        flip_y: false,
                    }),
                },
            ));
    }

    /// # Safety
    /// Source and destination are live CopyResource resource interfaces.
    pub unsafe fn copy_resource(
        &self,
        list: *mut c_void,
        destination: *mut c_void,
        source: *mut c_void,
    ) {
        if polymer_presentation::observation::CaptureScope::active() {
            return;
        }
        let Some(source) = (unsafe { Self::tracked_resource(source) }) else {
            return;
        };
        let Some(destination) = (unsafe { Self::tracked_resource(destination) }) else {
            return;
        };
        let desc = unsafe { source.resource.GetDesc() };
        for index in 0..u32::from(desc.MipLevels).max(1) * u32::from(desc.DepthOrArraySize) {
            if let (Some(source), Some(destination)) = (
                source.at_subresource(index),
                destination.at_subresource(index),
            ) {
                self.record_transfer(
                    list,
                    destination,
                    source,
                    polymer_presentation::Evidence::Copy,
                );
            }
        }
    }

    /// # Safety
    /// Both pointers must be live resource interfaces for this resolve command.
    pub unsafe fn resolve_subresource(
        &self,
        list: *mut c_void,
        destination: *mut c_void,
        destination_subresource: u32,
        source: *mut c_void,
        source_subresource: u32,
    ) {
        if polymer_presentation::observation::CaptureScope::active() {
            return;
        }
        let source = unsafe { Self::tracked_resource(source) }
            .and_then(|resource| resource.at_subresource(source_subresource));
        let destination = unsafe { Self::tracked_resource(destination) }
            .and_then(|resource| resource.at_subresource(destination_subresource));
        if let (Some(source), Some(destination)) = (source, destination) {
            self.record_transfer(
                list,
                destination,
                source,
                polymer_presentation::Evidence::Resolve,
            );
        }
    }

    fn record_transfer(
        &self,
        list: *mut c_void,
        destination: TrackedResource,
        source: TrackedResource,
        evidence: polymer_presentation::Evidence,
    ) {
        let extent = source.extent();
        if destination.extent() != extent {
            return;
        }
        let candidate = Candidate {
            evidence,
            region: D3d12CaptureRegion {
                x: 0,
                y: 0,
                width: extent.width,
                height: extent.height,
            },
            draw_area: extent.area(),
            source: Some(source),
            bindings: None,
            mapping: Some(RegionMapping {
                source: extent.rect(),
                destination: extent.rect(),
                flip_x: false,
                flip_y: false,
            }),
        };
        self.commands
            .entry(list as usize)
            .or_default()
            .observations
            .push((
                (
                    Self::identity_key(&destination.resource),
                    destination.subresource,
                ),
                candidate,
            ));
    }
}
