use super::*;
use windows::Win32::Graphics::Direct3D11::{ID3D11RasterizerState, ID3D11RenderTargetView};

#[derive(Clone)]
pub(super) struct RecordedObservation {
    pub edge: DependencyEdge<(usize, u32)>,
    pub source: TrackedResource,
    pub destination: TrackedResource,
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
        self.observations
            .forget_matching(|id| removed.contains(&id.0));
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
        let now = self.observations.generation();
        observation.edge.generation = now;
        observation.edge.order = self.observation_order.fetch_add(1, Ordering::Relaxed);
        self.resources
            .insert(observation.edge.source, observation.source);
        self.resources
            .insert(observation.edge.destination, observation.destination);
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
