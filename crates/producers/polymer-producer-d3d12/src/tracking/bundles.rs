use super::*;

/// None denotes inherited state; Some(empty/zero) denotes explicitly unbound
/// state. A signature/heap round trip must not resurrect invalidated arguments.
#[derive(Clone, Default)]
pub(super) struct Bindings {
    pub signature: Option<usize>,
    pub heaps: Option<Vec<u64>>,
    pub tables: HashMap<u32, u64>,
    invalidated: bool,
}
impl Bindings {
    pub fn set_signature(&mut self, signature: usize) {
        if self.signature != Some(signature) {
            self.invalidated |= self.signature.is_some();
            self.tables.clear();
            self.signature = Some(signature);
        }
    }
    pub fn set_heaps(&mut self, heaps: Vec<u64>) {
        if self.heaps.as_ref() != Some(&heaps) {
            self.invalidated |= self.heaps.is_some();
            self.tables.clear();
            self.heaps = Some(heaps);
        }
    }
    fn inherited(mut self, parent: &Self) -> Self {
        let signature = self.signature.or(parent.signature);
        let heaps = self.heaps.clone().or_else(|| parent.heaps.clone());
        if !self.invalidated && signature == parent.signature && heaps == parent.heaps {
            for (&index, &handle) in &parent.tables {
                self.tables.entry(index).or_insert(handle);
            }
        }
        self.signature = signature;
        self.heaps = heaps;
        self
    }
}

impl Tracking {
    pub fn execute_bundle(&self, parent: *mut c_void, bundle: *mut c_void) {
        if parent == bundle || polymer_presentation::observation::CaptureScope::active() {
            return;
        }
        let Some(bundle) = self
            .commands
            .get(&(bundle as usize))
            .map(|state| state.clone())
        else {
            return;
        };
        let mut state = self.commands.entry(parent as usize).or_default();
        let parent_bindings = state.bindings.clone();
        state
            .observations
            .extend(bundle.observations.into_iter().map(|(key, mut candidate)| {
                candidate.bindings = candidate
                    .bindings
                    .map(|bindings| bindings.inherited(&parent_bindings));
                (key, candidate)
            }));
        state.transitions.extend(bundle.transitions);
        for draw in bundle.inherited_draws {
            Self::inherit_bundle_draw(&mut state, draw);
        }
        // ExecuteBundle leaves its final state in the parent for later draws.
        state.bindings = bundle.bindings.inherited(&parent_bindings);
        if bundle.viewport_set {
            state.viewport = bundle.viewport;
            state.viewport_set = true;
        }
        if bundle.scissor_set {
            state.scissor = bundle.scissor;
            state.scissor_set = true;
        }
    }

    fn inherit_bundle_draw(state: &mut CommandState, draw: InheritedDraw) {
        let region = draw
            .scissor
            .or(state.scissor)
            .and_then(Self::region_from_scissor)
            .or_else(|| {
                draw.viewport
                    .or(state.viewport)
                    .and_then(Self::region_from_viewport)
            });
        let Some(region) = region else {
            return;
        };
        let bindings = draw.bindings.inherited(&state.bindings);
        for target in state.render_targets.clone() {
            Self::draw_target(state, target, region, Some(bindings.clone()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        tracking: Tracking,
        parent: *mut c_void,
        bundle: *mut c_void,
    }
    impl Fixture {
        fn new() -> Self {
            let tracking = Tracking::default();
            let parent = std::ptr::dangling_mut::<c_void>();
            let bundle = 2usize as *mut c_void;
            {
                let mut state = tracking.commands.entry(1).or_default();
                state.viewport = Some(D3D12_VIEWPORT {
                    Width: 640.0,
                    Height: 480.0,
                    ..Default::default()
                });
                state.render_targets.push(TrackedTarget {
                    key: (100, 0),
                    width: 640,
                    height: 480,
                });
                state.bindings.set_signature(7);
                state.bindings.set_heaps(vec![1000]);
                state.bindings.tables.insert(0, 1100);
                state.bindings.tables.insert(1, 1200);
            }
            Self {
                tracking,
                parent,
                bundle,
            }
        }
    }
    #[test]
    fn bundle_snapshots_bindings_per_draw_and_propagates_final_state() {
        let fixture = Fixture::new();
        let Fixture {
            tracking,
            parent,
            bundle,
        } = fixture;
        tracking.record_draw(bundle);
        tracking.record_root_signature(bundle, 7usize as *mut c_void);
        tracking.record_root_descriptor_table(bundle, 0, D3D12_GPU_DESCRIPTOR_HANDLE { ptr: 1300 });
        tracking.record_draw(bundle);
        let viewport = D3D12_VIEWPORT {
            Width: 320.0,
            Height: 200.0,
            ..Default::default()
        };
        unsafe {
            tracking.record_viewports(bundle, 1, &viewport);
        }
        tracking.execute_bundle(parent, bundle);
        tracking.record_draw(parent);
        let state = tracking.commands.get(&1).unwrap();
        let bindings = state.draw_bindings();
        assert_eq!(bindings[0].tables[&0], 1100);
        assert_eq!(bindings[1].tables[&0], 1300);
        assert_eq!(bindings[1].tables[&1], 1200);
        assert_eq!(bindings[2].tables[&0], 1300);
        assert_eq!(bindings[2].heaps, Some(vec![1000]));
        assert_eq!(state.viewport.unwrap().Width, 320.0);
    }
    #[test]
    fn signature_round_trip_does_not_resurrect_parent_arguments() {
        let Fixture {
            tracking,
            parent,
            bundle,
        } = Fixture::new();
        tracking.record_root_signature(bundle, 8usize as *mut c_void);
        tracking.record_root_signature(bundle, 7usize as *mut c_void);
        tracking.record_draw(bundle);
        tracking.execute_bundle(parent, bundle);
        tracking.record_draw(parent);
        let state = tracking.commands.get(&1).unwrap();
        let bindings = state.draw_bindings();
        assert!(bindings[0].tables.is_empty());
        assert!(bindings[1].tables.is_empty());
    }
    #[test]
    fn explicit_unbinding_is_not_inherited_and_empty_viewports_propagate() {
        let Fixture {
            tracking,
            parent,
            bundle,
        } = Fixture::new();
        unsafe {
            tracking.record_descriptor_heaps(bundle, 0, std::ptr::null());
        }
        tracking.record_draw(bundle);
        unsafe {
            tracking.record_viewports(bundle, 0, std::ptr::null());
        }
        tracking.execute_bundle(parent, bundle);
        tracking.record_draw(parent);
        let state = tracking.commands.get(&1).unwrap();
        let bindings = state.draw_bindings();
        assert_eq!(state.observations.len(), 1);
        assert_eq!(bindings[0].heaps, Some(vec![]));
        assert!(bindings[0].tables.is_empty());
        assert!(state.viewport.is_none());
    }
}
