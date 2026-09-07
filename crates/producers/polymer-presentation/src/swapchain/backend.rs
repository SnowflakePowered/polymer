use super::{FixedSurfaceBinding, state::*};
use crate::producer::ExportDescriptor;
use polymer_protocol::{ConsumerApi, ExternalImageKind, ExternalSyncKind, MAX_IMAGE_SLOTS};

/// The native frontend must validate every requested API feature against this
/// profile before handing any buffers to the guest.
#[derive(Clone, Debug)]
pub struct BackendProfile {
    pub image_kind: ExternalImageKind,
    pub sync_kind: ExternalSyncKind,
    pub format: u32,
    pub image_count: u32,
}

impl BackendProfile {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.image_count == 0 || self.image_count as usize > MAX_IMAGE_SLOTS {
            return Err("virtual image count exceeds existing transport");
        }
        Ok(())
    }
}

/// No Send/Sync requirement: GL implementations execute on their context owner.
/// Implementations retain native references in tokens/submissions, including
/// when a driver call might have succeeded before reporting an error.
pub trait SwapchainBackend: Sized {
    type CreateInfo;
    type ApiAccess;
    type AcquireToken;
    type AcquireInfo;
    type PresentInfo;
    type Submission;
    type UncertainWork;
    type Error;

    fn probe(
        info: &Self::CreateInfo,
        binding: &FixedSurfaceBinding,
        consumer: ConsumerApi,
    ) -> Result<BackendProfile, Self::Error>;
    fn create(
        info: Self::CreateInfo,
        binding: &FixedSurfaceBinding,
        profile: &BackendProfile,
    ) -> Result<Self, Self::Error>;
    fn export_descriptor(&self) -> ExportDescriptor<'_>;
    fn api_access(&self, slot: ImageSlot) -> Self::ApiAccess;
    fn poll_acquire(
        &mut self,
        slot: ImageSlot,
        prerequisite: &ReusePrerequisite,
        info: &Self::AcquireInfo,
    ) -> AcquirePoll<Self::AcquireToken, Self::UncertainWork, Self::Error>;
    fn submit_present(
        &mut self,
        token: Self::AcquireToken,
        info: Self::PresentInfo,
        signal: PublicationSignal,
    ) -> SubmitOutcome<Self::Submission, Self::AcquireToken, Self::UncertainWork, Self::Error>;
    fn cancel_acquire(
        &mut self,
        token: Self::AcquireToken,
    ) -> CancelOutcome<Self::Submission, Self::UncertainWork, Self::Error>;
    fn poll_progress(
        &mut self,
        submissions: &[SubmissionRef<'_, Self::Submission>],
    ) -> Result<BackendProgress, Self::Error>;

    /// Must prove GPU, app-reference and external-consumer lifetimes have ended.
    /// This is called only on the API owner. Pending work is kept, never dropped.
    fn poll_retirement(
        &mut self,
        work: &mut RetirementWork<Self>,
        reason: RetireReason,
    ) -> RetirementProgress;

    fn retire(
        self,
        work: RetirementWork<Self>,
        reason: RetireReason,
    ) -> super::BackendRetirement<Self> {
        super::BackendRetirement::new(self, work, reason)
    }
}
