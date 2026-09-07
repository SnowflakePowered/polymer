use super::{RetireReason, RetirementProgress, RetirementWork, SwapchainBackend};

/// Must be polled/finalized on the native API owner. Abandoning a retirement
/// deliberately retains the resource generation rather than calling GL or
/// destroying memory still referenced by a GPU or another process.
#[must_use = "retirement must be polled on the API owner"]
pub struct BackendRetirement<B: SwapchainBackend> {
    owned: Option<(B, RetirementWork<B>)>,
    permit: Option<super::budget::GenerationPermit>,
    reason: RetireReason,
    quarantined: bool,
}
impl<B: SwapchainBackend> BackendRetirement<B> {
    pub fn new(backend: B, work: RetirementWork<B>, reason: RetireReason) -> Self {
        Self {
            owned: Some((backend, work)),
            permit: None,
            reason,
            quarantined: false,
        }
    }
    pub(crate) fn with_permit(mut self, permit: super::budget::GenerationPermit) -> Self {
        self.permit = Some(permit);
        self
    }
    pub fn poll(&mut self) -> RetirementProgress {
        if self.quarantined {
            return RetirementProgress::Quarantined;
        }
        let Some((backend, work)) = self.owned.as_mut() else {
            return RetirementProgress::Safe;
        };
        let progress = backend.poll_retirement(work, self.reason);
        match progress {
            RetirementProgress::Safe => {
                self.owned.take();
                self.permit.take();
            }
            RetirementProgress::Quarantined => {
                self.quarantined = true;
            }
            RetirementProgress::Pending => {}
        }
        progress
    }
}
impl<B: SwapchainBackend> Drop for BackendRetirement<B> {
    fn drop(&mut self) {
        if let Some(owned) = self.owned.take() {
            // Owner-thread progress, not Drop, establishes finalization safety.
            std::mem::forget((owned, self.permit.take()));
        }
    }
}
