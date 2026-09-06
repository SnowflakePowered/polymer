use super::*;
use windows::Win32::Graphics::Direct3D11::{ID3D11RasterizerState, ID3D11RenderTargetView};

#[derive(Clone)]
pub(super) struct RecordedObservation {
    pub edge: DependencyEdge<(usize, u32)>,
    pub source: TrackedResource,
    pub destination: TrackedResource,
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::{
        Foundation::HMODULE,
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_WARP,
            Direct3D11::{
                D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION,
                D3D11_USAGE_DEFAULT, D3D11CreateDevice,
            },
            Dxgi::Common::{DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC},
        },
    };

    #[test]
    fn deferred_copy_is_visible_only_after_execution_and_replay_refreshes_it() {
        unsafe {
            let mut device = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_WARP,
                HMODULE::default(),
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
            let desc = D3D11_TEXTURE2D_DESC {
                Width: 64,
                Height: 64,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                ..Default::default()
            };
            let (mut source, mut destination) = (None, None);
            device
                .CreateTexture2D(&desc, None, Some(&mut source))
                .unwrap();
            device
                .CreateTexture2D(&desc, None, Some(&mut destination))
                .unwrap();
            let (source, destination) = (source.unwrap(), destination.unwrap());
            let tracking = Tracking::default();
            tracking.deferred_context_created(context.as_raw());
            context.CopyResource(&destination, &source);
            tracking.record_copy(
                context.as_raw(),
                destination.as_raw(),
                0,
                0,
                0,
                source.as_raw(),
                0,
                std::ptr::null(),
                false,
            );
            let mut list = None;
            context.FinishCommandList(false, Some(&mut list)).unwrap();
            let list = list.unwrap();
            tracking.finish_commands(context.as_raw(), list.as_raw(), false);
            assert!(tracking.observations.snapshot(0).1.is_empty());
            tracking.execute_commands(list.as_raw(), || {});
            let edges = tracking.observations.snapshot(0).1;
            assert_eq!(edges.len(), 1);
            tracking.execute_commands(list.as_raw(), || {});
            let replay = tracking.observations.snapshot(0).1;
            assert_eq!(replay.len(), 1);
            assert_eq!(replay[0].generation, edges[0].generation);
            assert!(replay[0].order > edges[0].order);
            assert_eq!(
                edges[0].source.0,
                source.cast::<IUnknown>().unwrap().as_raw() as usize
            );
            tracking.generation.store(4, Ordering::Relaxed);
            tracking.execute_commands(list.as_raw(), || {});
            assert_eq!(tracking.observations.snapshot(4).1[0].generation, 4);
            // Retiring a backbuffer must not erase unrelated command lists or
            // the live deferred-context registration and unaffected bindings.
            let mut other = None;
            device
                .CreateTexture2D(&desc, None, Some(&mut other))
                .unwrap();
            let other = other.unwrap();
            context.CopyResource(&destination, &other);
            tracking.record_copy(
                context.as_raw(),
                destination.as_raw(),
                0,
                0,
                0,
                other.as_raw(),
                0,
                std::ptr::null(),
                false,
            );
            let mut other_list = None;
            context
                .FinishCommandList(false, Some(&mut other_list))
                .unwrap();
            let other_list = other_list.unwrap();
            tracking.finish_commands(context.as_raw(), other_list.as_raw(), false);
            let (mut removed_view, mut kept_view) = (None, None);
            device
                .CreateShaderResourceView(&source, None, Some(&mut removed_view))
                .unwrap();
            device
                .CreateShaderResourceView(&other, None, Some(&mut kept_view))
                .unwrap();
            let (removed_view, kept_view) = (removed_view.unwrap(), kept_view.unwrap());
            tracking.shader_resources(
                context.as_raw(),
                0,
                2,
                [removed_view.as_raw(), kept_view.as_raw()].as_ptr(),
            );
            let removed = [source.cast::<IUnknown>().unwrap().as_raw() as usize]
                .into_iter()
                .collect();
            tracking.forget_resources(&removed);
            {
                let state = tracking.deferred.get(&(context.as_raw() as usize)).unwrap();
                assert!(state.bindings.views[0].is_none());
                assert!(state.bindings.views[1].is_some());
            }
            tracking.observations.clear();
            tracking.execute_commands(list.as_raw(), || {});
            tracking.execute_commands(other_list.as_raw(), || {});
            let remaining = tracking.observations.snapshot(4).1;
            assert_eq!(remaining.len(), 1);
            assert_eq!(
                remaining[0].source.0,
                other.cast::<IUnknown>().unwrap().as_raw() as usize
            );
            tracking.forget_commands(list.as_raw() as usize);
            tracking.observations.clear();
            tracking.execute_commands(list.as_raw(), || {});
            assert!(tracking.observations.snapshot(4).1.is_empty());
            // Context destruction discards bindings and unfinished work, but
            // independently owned immutable command lists remain executable.
            tracking.record_copy(
                context.as_raw(),
                destination.as_raw(),
                0,
                0,
                0,
                other.as_raw(),
                0,
                std::ptr::null(),
                false,
            );
            assert!(
                !tracking
                    .deferred
                    .get(&(context.as_raw() as usize))
                    .unwrap()
                    .observations
                    .is_empty()
            );
            tracking.forget_context(context.as_raw() as usize);
            assert!(!tracking.deferred.contains_key(&(context.as_raw() as usize)));
            tracking.deferred_context_created(context.as_raw());
            {
                let state = tracking.deferred.get(&(context.as_raw() as usize)).unwrap();
                assert!(state.observations.is_empty());
                assert!(state.bindings.views.is_empty());
            }
            tracking.execute_commands(other_list.as_raw(), || {});
            assert_eq!(tracking.observations.snapshot(4).1.len(), 1);
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct Bindings {
    pub targets: Vec<Option<ID3D11RenderTargetView>>,
    pub views: Vec<Option<ID3D11ShaderResourceView>>,
    viewport: Option<D3D11_VIEWPORT>,
    scissor: Option<windows::Win32::Foundation::RECT>,
    scissor_enabled: bool,
}

impl Bindings {
    fn forget_resources(&mut self, removed: &std::collections::HashSet<usize>) {
        for view in &mut self.targets {
            if Self::references_removed(view, removed) {
                *view = None;
            }
        }
        for view in &mut self.views {
            if Self::references_removed(view, removed) {
                *view = None;
            }
        }
    }
    fn references_removed<V: Interface>(
        view: &Option<V>,
        removed: &std::collections::HashSet<usize>,
    ) -> bool {
        view.as_ref()
            .and_then(|view| {
                view.cast::<windows::Win32::Graphics::Direct3D11::ID3D11View>()
                    .ok()
            })
            .and_then(|view| unsafe { view.GetResource() }.ok())
            .is_some_and(|resource| removed.contains(&Tracking::identity_key(&resource)))
    }

    pub fn region(&self) -> Option<D3d11CaptureRegion> {
        let viewport = self.viewport?;
        let region = float_region(
            viewport.TopLeftX,
            viewport.TopLeftY,
            viewport.TopLeftX + viewport.Width,
            viewport.TopLeftY + viewport.Height,
        )?;
        if !self.scissor_enabled {
            return Some(region);
        }
        let scissor = self.scissor?;
        let clip = float_region(
            scissor.left as f32,
            scissor.top as f32,
            scissor.right as f32,
            scissor.bottom as f32,
        )?;
        Some(Rect::from(region).intersect(clip.into())?.into())
    }
}

#[derive(Default)]
pub(super) struct DeferredState {
    pub bindings: Bindings,
    observations: Vec<RecordedObservation>,
}

impl Tracking {
    pub(super) fn forget_resources(&self, removed: &std::collections::HashSet<usize>) {
        let _observations = self.observations.transaction();
        self.resources.retain(|id, _| !removed.contains(&id.0));
        for &resource in removed {
            self.swapchain_targets.remove(&resource);
        }
        let (targets, edges) = self
            .observations
            .snapshot(self.generation.load(Ordering::Relaxed));
        let ids = targets
            .into_iter()
            .map(|target| target.id)
            .chain(
                edges
                    .into_iter()
                    .flat_map(|edge| [edge.source, edge.destination]),
            )
            .filter(|id| removed.contains(&id.0))
            .collect::<std::collections::HashSet<_>>();
        for id in ids {
            self.observations.forget(id);
        }
        let keep = |value: &RecordedObservation| {
            !removed.contains(&value.edge.source.0) && !removed.contains(&value.edge.destination.0)
        };
        for mut state in self.deferred.iter_mut() {
            state.bindings.forget_resources(removed);
            state.observations.retain(&keep);
        }
        for mut batch in self.command_lists.iter_mut() {
            batch.retain(&keep);
        }
    }

    pub(super) fn observe(&self, context: &ID3D11DeviceContext, observation: RecordedObservation) {
        if unsafe { context.GetType() }
            == windows::Win32::Graphics::Direct3D11::D3D11_DEVICE_CONTEXT_IMMEDIATE
        {
            self.commit(observation);
        } else {
            self.deferred
                .entry(context.as_raw() as usize)
                .or_default()
                .observations
                .push(observation);
        }
    }

    fn commit(&self, mut observation: RecordedObservation) {
        let now = self.generation.load(Ordering::Relaxed);
        observation.edge.generation = now;
        observation.edge.order = self.observation_order.fetch_add(1, Ordering::Relaxed);
        self.resources
            .insert(observation.edge.source, (observation.source, now));
        self.resources
            .insert(observation.edge.destination, (observation.destination, now));
        if matches!(
            observation.edge.evidence,
            polymer_presentation::Evidence::SampledDraw
                | polymer_presentation::Evidence::RecoveredQuad
                | polymer_presentation::Evidence::Clear
        ) {
            self.observations.target(
                Target {
                    id: observation.edge.destination,
                    score: if observation.edge.evidence == Evidence::Clear {
                        0
                    } else {
                        observation
                            .edge
                            .coverage
                            .map_or(0, |rect| rect.extent.area())
                    },
                    order: observation.edge.order,
                },
                now,
            );
        }
        self.observations.record(observation.edge);
    }

    pub fn deferred_context_created(&self, context: *mut c_void) {
        self.deferred.insert(context as usize, Default::default());
    }

    pub fn forget_context(&self, context: usize) {
        self.deferred.remove(&context);
    }

    /// # Safety
    /// `views` references `count` valid shader-resource-view interface pointers.
    pub unsafe fn shader_resources(
        &self,
        context: *mut c_void,
        start: u32,
        count: u32,
        views: *const *mut c_void,
    ) {
        if views.is_null() || start >= 128 {
            return;
        }
        let Some(mut state) = self.deferred.get_mut(&(context as usize)) else {
            return;
        };
        state.bindings.views.resize(128, None);
        for (index, &raw) in
            unsafe { std::slice::from_raw_parts(views, count.min(128 - start) as usize) }
                .iter()
                .enumerate()
        {
            state.bindings.views[start as usize + index] =
                unsafe { ID3D11ShaderResourceView::from_raw_borrowed(&raw) }.cloned();
        }
    }

    /// # Safety
    /// `views` references `count` valid render-target-view interface pointers.
    pub unsafe fn render_targets(
        &self,
        context: *mut c_void,
        count: u32,
        views: *const *mut c_void,
    ) {
        let Some(mut state) = self.deferred.get_mut(&(context as usize)) else {
            return;
        };
        state.bindings.targets.clear();
        if views.is_null() {
            return;
        }
        state.bindings.targets.extend(
            unsafe { std::slice::from_raw_parts(views, count.min(8) as usize) }
                .iter()
                .map(|&raw| unsafe { ID3D11RenderTargetView::from_raw_borrowed(&raw) }.cloned()),
        );
    }

    /// # Safety
    /// `value` is null or references a live viewport.
    pub unsafe fn viewport(&self, context: *mut c_void, count: u32, value: *const D3D11_VIEWPORT) {
        let Some(mut state) = self.deferred.get_mut(&(context as usize)) else {
            return;
        };
        state.bindings.viewport = (count != 0 && !value.is_null()).then(|| unsafe { *value });
    }

    /// # Safety
    /// `value` is null or references a live scissor rectangle.
    pub unsafe fn scissor(
        &self,
        context: *mut c_void,
        count: u32,
        value: *const windows::Win32::Foundation::RECT,
    ) {
        let Some(mut state) = self.deferred.get_mut(&(context as usize)) else {
            return;
        };
        state.bindings.scissor = (count != 0 && !value.is_null()).then(|| unsafe { *value });
    }

    /// # Safety
    /// `raw` is null or a live rasterizer state.
    pub unsafe fn rasterizer(&self, context: *mut c_void, raw: *mut c_void) {
        let mut desc = D3D11_RASTERIZER_DESC::default();
        if let Some(state) = unsafe { ID3D11RasterizerState::from_raw_borrowed(&raw) } {
            unsafe { state.GetDesc(&mut desc) };
        }
        if let Some(mut state) = self.deferred.get_mut(&(context as usize)) {
            state.bindings.scissor_enabled = desc.ScissorEnable.as_bool();
        }
    }

    pub fn clear_context(&self, context: *mut c_void) {
        if let Some(mut state) = self.deferred.get_mut(&(context as usize)) {
            state.bindings = Bindings::default();
        }
    }

    pub fn finish_commands(&self, context: *mut c_void, list: *mut c_void, restore: bool) {
        let Some(mut state) = self.deferred.get_mut(&(context as usize)) else {
            return;
        };
        self.command_lists
            .insert(list as usize, std::mem::take(&mut state.observations));
        if !restore {
            state.bindings = Bindings::default();
        }
    }

    /// Keeps native list execution and its immutable observation batch ordered
    /// together with presentation snapshots and other list executions.
    pub fn execute_commands(&self, list: *mut c_void, submit: impl FnOnce()) {
        if polymer_presentation::observation::CaptureScope::active() {
            submit();
            return;
        }
        let _observations = self.observations.transaction();
        submit();
        if let Some(batch) = self.command_lists.get(&(list as usize)) {
            for observation in batch.iter().cloned() {
                self.commit(observation);
            }
        }
    }

    pub fn forget_commands(&self, list: usize) {
        self.command_lists.remove(&list);
    }
}
