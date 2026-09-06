use super::{CommandState, D3d12CaptureSourceState, Tracking};
use std::ffi::c_void;
use windows::Win32::Graphics::Direct3D12::*;

impl CommandState {
    fn enhanced_transition(
        &mut self,
        resource: usize,
        desc: D3D12_RESOURCE_DESC,
        barrier: &D3D12_TEXTURE_BARRIER,
    ) {
        let state = (!barrier.SyncAfter.contains(D3D12_BARRIER_SYNC_SPLIT)).then_some(
            D3d12CaptureSourceState::Enhanced {
                sync: barrier.SyncAfter,
                access: barrier.AccessAfter,
                layout: barrier.LayoutAfter,
            },
        );
        let range = barrier.Subresources;
        if range.NumMipLevels == 0 {
            self.transitions
                .push(((resource, range.IndexOrFirstMipLevel), state));
            return;
        }
        let mips = u32::from(desc.MipLevels);
        let arrays = if desc.Dimension == D3D12_RESOURCE_DIMENSION_TEXTURE3D {
            1
        } else {
            u32::from(desc.DepthOrArraySize)
        };
        let Some(indices) = BarrierSubresources::new(range, mips, arrays) else {
            // Malformed/unknown ranges cannot establish a safe capture state.
            self.transitions.push(((resource, u32::MAX), None));
            return;
        };
        self.transitions
            .extend(indices.map(|index| ((resource, index), state)));
    }
}

struct BarrierSubresources {
    mips: u32,
    arrays: u32,
    range: D3D12_BARRIER_SUBRESOURCE_RANGE,
    position: u32,
    count: u32,
}

impl BarrierSubresources {
    fn new(range: D3D12_BARRIER_SUBRESOURCE_RANGE, mips: u32, arrays: u32) -> Option<Self> {
        if range.IndexOrFirstMipLevel.checked_add(range.NumMipLevels)? > mips
            || range.FirstArraySlice.checked_add(range.NumArraySlices)? > arrays
            || range.NumArraySlices == 0
            || range.NumPlanes == 0
        {
            return None;
        }
        range
            .FirstPlane
            .checked_add(range.NumPlanes)?
            .checked_mul(arrays)?
            .checked_mul(mips)?;
        let count = range
            .NumMipLevels
            .checked_mul(range.NumArraySlices)?
            .checked_mul(range.NumPlanes)?;
        Some(Self {
            mips,
            arrays,
            range,
            position: 0,
            count,
        })
    }
}

impl Iterator for BarrierSubresources {
    type Item = u32;

    fn next(&mut self) -> Option<u32> {
        if self.position == self.count {
            return None;
        }
        let mip = self.range.IndexOrFirstMipLevel + self.position % self.range.NumMipLevels;
        let slice = self.position / self.range.NumMipLevels;
        let array = self.range.FirstArraySlice + slice % self.range.NumArraySlices;
        let plane = self.range.FirstPlane + slice / self.range.NumArraySlices;
        self.position += 1;
        Some(mip + array * self.mips + plane * self.mips * self.arrays)
    }
}

impl Tracking {
    /// Records legacy resource-state transitions without inferring draw bindings.
    ///
    /// # Safety
    ///
    /// `barriers` must be null or reference `count` valid barriers.
    pub unsafe fn record_resource_barrier(
        &self,
        list: *mut c_void,
        count: u32,
        barriers: *const D3D12_RESOURCE_BARRIER,
    ) {
        if polymer_presentation::observation::CaptureScope::active()
            || count == 0
            || barriers.is_null()
        {
            return;
        }
        let barriers = unsafe { std::slice::from_raw_parts(barriers, count as usize) };
        for barrier in barriers {
            if barrier.Type != D3D12_RESOURCE_BARRIER_TYPE_TRANSITION {
                continue;
            }
            let transition = unsafe { &*barrier.Anonymous.Transition };
            if let Some(resource) = transition.pResource.as_ref() {
                self.commands
                    .entry(list as usize)
                    .or_default()
                    .transitions
                    .push((
                        (Self::identity_key(resource), transition.Subresource),
                        (!barrier.Flags.contains(windows::Win32::Graphics::Direct3D12::D3D12_RESOURCE_BARRIER_FLAG_BEGIN_ONLY)).then_some(D3d12CaptureSourceState::Legacy(transition.StateAfter)),
                    ));
            }
        }
    }

    /// Records enhanced-barrier resource states without inferring draw bindings.
    ///
    /// # Safety
    ///
    /// `groups` must be null or reference `count` valid barrier groups.
    pub unsafe fn record_enhanced_barrier(
        &self,
        list: *mut c_void,
        count: u32,
        groups: *const D3D12_BARRIER_GROUP,
    ) {
        if polymer_presentation::observation::CaptureScope::active()
            || count == 0
            || groups.is_null()
        {
            return;
        }
        let groups = unsafe { std::slice::from_raw_parts(groups, count as usize) };
        for group in groups {
            if group.Type != D3D12_BARRIER_TYPE_TEXTURE {
                continue;
            }
            let texture_barriers = unsafe { group.Anonymous.pTextureBarriers };
            if texture_barriers.is_null() {
                continue;
            }
            for barrier in
                unsafe { std::slice::from_raw_parts(texture_barriers, group.NumBarriers as usize) }
            {
                if let Some(resource) = barrier.pResource.as_ref() {
                    self.commands
                        .entry(list as usize)
                        .or_default()
                        .enhanced_transition(
                            Self::identity_key(resource),
                            unsafe { resource.GetDesc() },
                            barrier,
                        );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enhanced_ranges_and_split_submission_preserve_sibling_capture_states() {
        let tracking = Tracking::default();
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            MipLevels: 4,
            DepthOrArraySize: 3,
            ..Default::default()
        };
        let mut barrier = D3D12_TEXTURE_BARRIER {
            SyncAfter: D3D12_BARRIER_SYNC_COPY,
            AccessAfter: D3D12_BARRIER_ACCESS_COPY_SOURCE,
            LayoutAfter: D3D12_BARRIER_LAYOUT_COPY_SOURCE,
            Subresources: D3D12_BARRIER_SUBRESOURCE_RANGE {
                IndexOrFirstMipLevel: u32::MAX,
                ..Default::default()
            },
            ..Default::default()
        };
        tracking
            .commands
            .entry(1)
            .or_default()
            .enhanced_transition(42, desc, &barrier);
        assert!(tracking.capture_state((42, 5), 7).is_none());
        tracking.commit_list(1, 7);
        assert!(tracking.capture_state((42, 5), 7).is_some());

        barrier.SyncAfter = D3D12_BARRIER_SYNC_SPLIT;
        barrier.Subresources = D3D12_BARRIER_SUBRESOURCE_RANGE {
            IndexOrFirstMipLevel: 1,
            NumMipLevels: 2,
            FirstArraySlice: 1,
            NumArraySlices: 2,
            FirstPlane: 1,
            NumPlanes: 2,
        };
        tracking
            .commands
            .entry(2)
            .or_default()
            .enhanced_transition(42, desc, &barrier);
        tracking.commit_list(2, 7);
        let affected = [17, 18, 21, 22, 29, 30, 33, 34];
        for index in 0..36 {
            assert_eq!(
                tracking.capture_state((42, index), 7).is_none(),
                affected.contains(&index)
            );
        }
        // The final half stays invisible until executed. Resetting it leaves
        // the affected range pending; recording/replaying it completes only that range.
        barrier.SyncBefore = D3D12_BARRIER_SYNC_SPLIT;
        barrier.SyncAfter = D3D12_BARRIER_SYNC_COPY;
        tracking
            .commands
            .entry(3)
            .or_default()
            .enhanced_transition(42, desc, &barrier);
        tracking.command_reset(3usize as *mut c_void);
        tracking.commit_list(3, 7);
        assert!(tracking.capture_state((42, 17), 7).is_none());
        tracking
            .commands
            .entry(3)
            .or_default()
            .enhanced_transition(42, desc, &barrier);
        tracking.commit_list(3, 7);
        for index in affected {
            assert!(tracking.capture_state((42, index), 7).is_some());
            assert!(tracking.capture_state((42, index), 8).is_none());
        }
        // A direct subresource index ignores all range fields.
        barrier.SyncAfter = D3D12_BARRIER_SYNC_SPLIT;
        barrier.Subresources.IndexOrFirstMipLevel = 5;
        barrier.Subresources.NumMipLevels = 0;
        tracking
            .commands
            .entry(4)
            .or_default()
            .enhanced_transition(42, desc, &barrier);
        tracking.commit_list(4, 7);
        assert!(tracking.capture_state((42, 5), 7).is_none());
        assert!(tracking.capture_state((42, 6), 7).is_some());
        // A later all-subresource transition clears pending per-layer overrides.
        tracking.commit_list(1, 7);
        assert!(tracking.capture_state((42, 5), 7).is_some());

        // A 3D texture's depth is not an array count. Its next mip belongs to
        // the same subresource chain, even when the depth exceeds one.
        barrier.SyncAfter = D3D12_BARRIER_SYNC_COPY;
        barrier.Subresources = D3D12_BARRIER_SUBRESOURCE_RANGE {
            IndexOrFirstMipLevel: 1,
            NumMipLevels: 2,
            NumArraySlices: 1,
            FirstPlane: 1,
            NumPlanes: 1,
            ..Default::default()
        };
        let mut volume = CommandState::default();
        volume.enhanced_transition(
            43,
            D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE3D,
                DepthOrArraySize: 16,
                ..desc
            },
            &barrier,
        );
        assert_eq!(
            volume
                .transitions
                .iter()
                .map(|(key, _)| key.1)
                .collect::<Vec<_>>(),
            [5, 6]
        );
    }
}
