use super::*;
impl Drop for D3d11Frontend {
    fn drop(&mut self) {
        let Some(state) = self.state.take().and_then(OwnerCell::into_inner) else {
            return;
        };
        DxgiMaintenance::retire(state.controller.retire(RetireReason::ApplicationDestroyed));
    }
}
