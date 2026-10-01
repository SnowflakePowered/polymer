use super::*;
impl D3d12Frontend {
    pub(super) fn refresh_latency(&self, state: &mut State) {
        state.failed = state.failed || !self.pacing.refresh(&mut state.controller);
        if state.failed {
            self.pacing.signal(true);
        }
    }
}
