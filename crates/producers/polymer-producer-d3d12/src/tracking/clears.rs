use super::*;

impl Tracking {
    /// Records the explicit RTV write without inheriting draw bindings.
    /// # Safety
    /// `rects` must reference `count` rectangles when count is nonzero.
    pub unsafe fn record_clear(
        &self,
        list: *mut c_void,
        handle: D3D12_CPU_DESCRIPTOR_HANDLE,
        count: u32,
        rects: *const RECT,
    ) {
        if polymer_presentation::observation::CaptureScope::active()
            || (count != 0 && rects.is_null())
        {
            return;
        }
        let Some(targets) = self
            .rtv_descriptors
            .get(&handle.ptr)
            .map(|targets| targets.clone())
        else {
            return;
        };
        let rectangles = if count == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(rects, count as usize) }
        };
        let mut state = self.commands.entry(list as usize).or_default();
        for target in targets {
            let full = Extent2D {
                width: target.width,
                height: target.height,
            }
            .rect();
            let coverage = if rectangles.is_empty() {
                vec![full]
            } else {
                rectangles
                    .iter()
                    .filter_map(|rect| {
                        polymer_presentation::Rect {
                            x: rect.left,
                            y: rect.top,
                            extent: Extent2D {
                                width: u32::try_from(i64::from(rect.right) - i64::from(rect.left))
                                    .ok()?,
                                height: u32::try_from(i64::from(rect.bottom) - i64::from(rect.top))
                                    .ok()?,
                            },
                        }
                        .intersect(full)
                    })
                    .collect()
            };
            for rect in coverage {
                let start = state.observations.len();
                Self::draw_target(
                    &mut state,
                    target,
                    D3d12CaptureRegion {
                        x: rect.x as u32,
                        y: rect.y as u32,
                        width: rect.extent.width,
                        height: rect.extent.height,
                    },
                    None,
                );
                for (_, candidate) in &mut state.observations[start..] {
                    candidate.evidence = polymer_presentation::Evidence::Clear;
                    candidate.draw_area = 0;
                }
            }
        }
    }
}
