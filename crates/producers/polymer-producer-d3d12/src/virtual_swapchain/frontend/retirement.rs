use super::*;
impl D3d12Frontend {
    pub(super) fn take_retirement(&mut self) -> Option<FrontendRetirement> {
        let mut state = self.state.take()?.into_inner()?;
        for lease in state.leases.iter_mut().filter_map(Option::take) {
            let _ = state.controller.cancel(lease);
        }
        Some(state.controller.retire(RetireReason::ApplicationDestroyed))
    }
}
impl Drop for D3d12Frontend {
    fn drop(&mut self) {
        let Some(retirement) = self.take_retirement() else {
            return;
        };
        DxgiMaintenance::retire(retirement);
    }
}
