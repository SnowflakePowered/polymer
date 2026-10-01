use super::*;
impl D3d11Frontend {
    pub(crate) fn bind(
        &mut self,
        registration: SourceRegistration,
        window: polymer_platform_windows::FixedWindowBinding,
    ) -> windows::core::Result<()> {
        self.registration = Some(registration);
        self.fixed_window = Some(window);
        Ok(())
    }
    pub(super) fn refresh_latency(&self, state: &mut State) {
        state.failed = state.failed || !self.pacing.refresh(&mut state.controller);
        if state.failed {
            self.pacing.signal(true);
        }
    }
}
