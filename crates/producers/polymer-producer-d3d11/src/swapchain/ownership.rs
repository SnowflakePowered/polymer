use super::*;

pub(super) struct ImmediateAccess(ID3D11Multithread);
impl ImmediateAccess {
    pub fn enter(gate: &ID3D11Multithread) -> Self {
        unsafe {
            gate.Enter();
        }
        Self(gate.clone())
    }
}
impl Drop for ImmediateAccess {
    fn drop(&mut self) {
        unsafe {
            self.0.Leave();
        }
    }
}

impl D3d11VirtualBackend {
    pub(super) fn release(
        &self,
        token: WriteToken,
        fence: &ID3D11Fence,
        value: u64,
    ) -> Result<Submission, (WriteToken, Error)> {
        let _access = ImmediateAccess::enter(&self.gate);
        if let Err(error) = unsafe { self.context.Signal(fence, value) } {
            return Err((token, error));
        }
        // A virtual present does not call native DXGI Present, so explicitly
        // submit the D3D11 batch. Signal completion orders all earlier writes.
        unsafe {
            self.context.Flush();
        }
        if let Err(error) = unsafe { self.device.GetDeviceRemovedReason() } {
            return Err((token, error));
        }
        Ok(Submission {
            fence: fence.clone(),
            value,
        })
    }

    pub(super) fn retirement_progress(
        &mut self,
        work: &mut RetirementWork<Self>,
    ) -> RetirementProgress {
        let _access = ImmediateAccess::enter(&self.gate);
        if !work.uncertain.is_empty() || unsafe { self.device.GetDeviceRemovedReason() }.is_err() {
            return RetirementProgress::Quarantined;
        }
        while let Some((slot, token)) = work.acquired.pop() {
            match self.cancel_acquire(token) {
                CancelOutcome::Released => {}
                CancelOutcome::Submitted(submission) => work.submissions.push((slot, submission)),
                CancelOutcome::Indeterminate {
                    work: uncertain, ..
                } => {
                    work.uncertain.push(uncertain);
                    return RetirementProgress::Quarantined;
                }
            }
        }
        for (_, submission) in &work.submissions {
            let value = unsafe { submission.fence.GetCompletedValue() };
            if value == u64::MAX {
                return RetirementProgress::Quarantined;
            }
            if value < submission.value {
                return RetirementProgress::Pending;
            }
        }
        self.retire_publications(work)
    }

    fn retire_publications(&self, work: &mut RetirementWork<Self>) -> RetirementProgress {
        if let Some(fence) = self.ring.consumer_fence() {
            let value = unsafe { fence.GetCompletedValue() };
            if value == u64::MAX {
                return RetirementProgress::Quarantined;
            }
            if work.publications.iter().any(|(_, needed)| value < *needed) {
                return RetirementProgress::Pending;
            }
        }
        RetirementProgress::Safe
    }
}
