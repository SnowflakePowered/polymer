use super::*;
use crate::producer::ProducerSession;
use polymer_protocol::{ConsumerApi, FrameReady, RELEASE_ALL_SYNC_VALUE};
use std::{io, sync::Arc};

#[derive(Debug)]
pub enum ControllerError<E> {
    Backend(E),
    Transport(io::Error),
    Contract(&'static str),
    Retiring(RetireReason),
    WouldBlock,
    Timeout,
}

pub struct CreationFailure<B: SwapchainBackend> {
    pub error: ControllerError<B::Error>,
    pub retirement: Option<BackendRetirement<B>>,
}

/// One immutable resource generation and one accepted IPC connection. Call on
/// the backend owner; a hook's owner container must take this value out before
/// calling it, leaving a Busy marker visible to reentrant calls. None of these
/// methods holds a map guard or shared state lock across driver calls or IPC.
pub struct SwapchainController<B: SwapchainBackend> {
    backend: Option<B>,
    permit: Option<super::budget::GenerationPermit>,
    identity: Arc<()>,
    publisher: Option<ProducerSession>,
    binding: Arc<FixedSurfaceBinding>,
    profile: BackendProfile,
    slots: Vec<SlotState<B>>,
    uncertain: Vec<B::UncertainWork>,
    next_value: u64,
    next_lease: u64,
    next_candidate: usize,
    retiring: Option<RetireReason>,
    consumer_release: u64,
    #[cfg(windows)]
    progress: Option<ProgressEvent>,
}

impl<B: SwapchainBackend> SwapchainController<B> {
    pub fn create(
        info: B::CreateInfo,
        binding: Arc<FixedSurfaceBinding>,
        publisher: ProducerSession,
    ) -> Result<Self, CreationFailure<B>> {
        let consumer = publisher.preferred_consumer();
        Self::build(info, binding, consumer, Some(publisher))
    }

    /// A generation with no viewer connection. Presents complete without being published.
    pub fn detached(
        info: B::CreateInfo,
        binding: Arc<FixedSurfaceBinding>,
        consumer: ConsumerApi,
    ) -> Result<Self, CreationFailure<B>> {
        Self::build(info, binding, consumer, None)
    }

    fn build(
        info: B::CreateInfo,
        binding: Arc<FixedSurfaceBinding>,
        consumer: ConsumerApi,
        mut publisher: Option<ProducerSession>,
    ) -> Result<Self, CreationFailure<B>> {
        let permit = super::budget::GenerationPermit::acquire().ok_or(CreationFailure {
            error: ControllerError::Contract(
                "virtual resource generation budget exhausted; retire or recreate the process",
            ),
            retirement: None,
        })?;
        let profile = B::probe(&info, &binding, consumer).map_err(|error| CreationFailure {
            error: ControllerError::Backend(error),
            retirement: None,
        })?;
        profile.validate().map_err(|error| CreationFailure {
            error: ControllerError::Contract(error),
            retirement: None,
        })?;
        let backend = B::create(info, &binding, &profile).map_err(|error| CreationFailure {
            error: ControllerError::Backend(error),
            retirement: None,
        })?;
        if let Some(Err(error)) = publisher
            .as_mut()
            .map(|publisher| publisher.publish(backend.export_descriptor()))
        {
            return Err(CreationFailure {
                error: ControllerError::Transport(error),
                retirement: Some(
                    backend
                        .retire(
                            RetirementWork {
                                submissions: Vec::new(),
                                acquired: Vec::new(),
                                uncertain: Vec::new(),
                                publications: Vec::new(),
                            },
                            RetireReason::ConnectionLost,
                        )
                        .with_permit(permit),
                ),
            });
        }
        let slots = (0..profile.image_count)
            .map(|_| SlotState::default())
            .collect();
        Ok(Self {
            backend: Some(backend),
            permit: Some(permit),
            identity: Arc::new(()),
            publisher,
            binding,
            profile,
            slots,
            uncertain: Vec::new(),
            next_value: 0,
            next_lease: 0,
            next_candidate: 0,
            retiring: None,
            consumer_release: 0,
            #[cfg(windows)]
            progress: ProgressEvent::new().ok(),
        })
    }

    pub fn profile(&self) -> &BackendProfile {
        &self.profile
    }
    pub fn backend(&self) -> &B {
        self.backend.as_ref().expect("live controller")
    }
    /// The oldest publication the consumer has not yet released.
    pub fn pending_release(&self) -> Option<u64> {
        self.slots
            .iter()
            .filter_map(|slot| slot.publication)
            .filter(|&value| value > self.consumer_release)
            .min()
    }
    /// The release value at which `pacing_ready(maximum)` becomes true.
    pub fn pacing_release_target(&self, maximum: u32) -> Option<u64> {
        let mut pending = self
            .slots
            .iter()
            .filter_map(|slot| slot.publication)
            .filter(|&value| value > self.consumer_release)
            .collect::<Vec<_>>();
        let excess = pending.len().checked_sub(maximum as usize)?;
        pending.sort_unstable();
        excess.checked_sub(1).map(|index| pending[index])
    }
    pub fn api_access(&self, slot: ImageSlot) -> Result<B::ApiAccess, ControllerError<B::Error>> {
        if slot.index() >= self.profile.image_count {
            return Err(ControllerError::Contract(
                "image slot belongs to another ring",
            ));
        }
        Ok(self
            .backend
            .as_ref()
            .expect("live controller")
            .api_access(slot))
    }

    fn available(&mut self) -> Result<(), ControllerError<B::Error>> {
        if let Some(reason) = self.retiring {
            return Err(ControllerError::Retiring(reason));
        }
        if self
            .publisher
            .as_ref()
            .is_some_and(|publisher| !publisher.connected())
        {
            self.retiring = Some(RetireReason::ConnectionLost);
            return Err(ControllerError::Retiring(RetireReason::ConnectionLost));
        }
        Ok(())
    }

    pub fn poll(&mut self) -> Result<(), ControllerError<B::Error>> {
        self.available()?;
        let submissions = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(index, state)| {
                state.submission.as_ref().map(|submission| SubmissionRef {
                    slot: ImageSlot::new(index as u32, self.profile.image_count).unwrap(),
                    submission,
                })
            })
            .collect::<Vec<_>>();
        let result = self.backend.as_mut().unwrap().poll_progress(&submissions);
        let progress = match result {
            Ok(progress) => progress,
            Err(error) => {
                self.retiring = Some(RetireReason::UncertainOperation);
                return Err(ControllerError::Backend(error));
            }
        };
        if progress.device_lost {
            self.retiring = Some(RetireReason::DeviceLost);
            return Err(ControllerError::Retiring(RetireReason::DeviceLost));
        }
        self.consumer_release = self.consumer_release.max(progress.released);
        for slot in progress.completed {
            let Some(state) = self.slots.get_mut(slot.index() as usize) else {
                self.retiring = Some(RetireReason::UncertainOperation);
                return Err(ControllerError::Contract(
                    "backend completed a foreign slot",
                ));
            };
            state.producer_complete = true;
        }
        Ok(())
    }

    /// DXGI can request a specific logical image, Vulkan can acquire any free
    /// image. A retained displayed image does not block other ring candidates.
    pub fn acquire(
        &mut self,
        requested: Option<ImageSlot>,
        deadline: AcquireDeadline,
    ) -> Result<AppLease, ControllerError<B::Error>>
    where
        B::AcquireInfo: Default,
    {
        self.acquire_paced(requested, deadline, None)
    }

    /// The transport retains the latest displayed publication. The latency
    /// allowance limits additional queued frames without requiring that last
    /// displayed image to be released before its replacement can be produced.
    pub fn acquire_paced(
        &mut self,
        requested: Option<ImageSlot>,
        deadline: AcquireDeadline,
        maximum_latency: Option<u32>,
    ) -> Result<AppLease, ControllerError<B::Error>>
    where
        B::AcquireInfo: Default,
    {
        self.acquire_with_info(
            requested,
            deadline,
            maximum_latency,
            &B::AcquireInfo::default(),
        )
    }

    pub fn acquire_with_info(
        &mut self,
        requested: Option<ImageSlot>,
        deadline: AcquireDeadline,
        maximum_latency: Option<u32>,
        info: &B::AcquireInfo,
    ) -> Result<AppLease, ControllerError<B::Error>> {
        if maximum_latency == Some(0) {
            return Err(ControllerError::Contract("frame latency must be nonzero"));
        }
        loop {
            self.poll()?;
            let paced = maximum_latency.is_none_or(|maximum| self.pacing_ready(maximum));
            if paced {
                if let Some(lease) = self.try_acquire(requested, info)? {
                    return Ok(lease);
                }
            }
            let release = match maximum_latency {
                Some(maximum) if !paced => self.pacing_release_target(maximum),
                _ => self.pending_release(),
            };
            self.wait_for_progress(deadline, release)?;
        }
    }
    /// Blocks until `release` is released or the oldest handoff completes.
    /// Backends without native events fall back to pausing.
    fn wait_for_progress(
        &self,
        deadline: AcquireDeadline,
        release: Option<u64>,
    ) -> Result<(), ControllerError<B::Error>> {
        let AcquireDeadline::Until(until) = deadline else {
            return Err(ControllerError::WouldBlock);
        };
        if deadline.expired() {
            return Err(ControllerError::Timeout);
        }
        #[cfg(windows)]
        if let Some(event) = &self.progress
            && self
                .backend()
                .arm_progress(release, self.oldest_in_flight(), event.handle())
                .map_err(ControllerError::Backend)?
        {
            event.wait(until.saturating_duration_since(std::time::Instant::now()));
            return Ok(());
        }
        #[cfg(not(windows))]
        let _ = release;
        deadline.pause();
        Ok(())
    }
    pub fn pacing_ready(&self, maximum: u32) -> bool {
        maximum != 0
            && self
                .slots
                .iter()
                .filter(|slot| {
                    slot.publication
                        .is_some_and(|value| value > self.consumer_release)
                })
                .count()
                <= maximum as usize
    }
    pub fn oldest_in_flight(&self) -> Option<&B::Submission> {
        self.slots
            .iter()
            .filter(|slot| slot.token.is_none() && !slot.producer_complete)
            .filter(|slot| slot.submission.is_some())
            .min_by_key(|slot| slot.serial)
            .and_then(|slot| slot.submission.as_ref())
    }
    pub fn wait_for_pacing(
        &mut self,
        maximum: u32,
        deadline: AcquireDeadline,
    ) -> Result<(), ControllerError<B::Error>> {
        if maximum == 0 {
            return Err(ControllerError::Contract("frame latency must be nonzero"));
        }
        loop {
            self.poll()?;
            if self.pacing_ready(maximum) {
                return Ok(());
            }
            self.wait_for_progress(deadline, self.pacing_release_target(maximum))?;
        }
    }

    fn try_acquire(
        &mut self,
        requested: Option<ImageSlot>,
        info: &B::AcquireInfo,
    ) -> Result<Option<AppLease>, ControllerError<B::Error>> {
        if requested.is_some_and(|slot| slot.index() >= self.profile.image_count) {
            return Err(ControllerError::Contract(
                "requested slot outside this ring",
            ));
        }
        let count = requested.map_or(self.slots.len(), |_| 1);
        for offset in 0..count {
            let index = requested
                .map_or((self.next_candidate + offset) % self.slots.len(), |slot| {
                    slot.index() as usize
                });
            if let Some(lease) = self.acquire_slot(index, info)? {
                self.next_candidate = (index + 1) % self.slots.len();
                return Ok(Some(lease));
            }
        }
        Ok(None)
    }

    fn acquire_slot(
        &mut self,
        index: usize,
        info: &B::AcquireInfo,
    ) -> Result<Option<AppLease>, ControllerError<B::Error>> {
        let state = &mut self.slots[index];
        if state.token.is_some() || !state.producer_complete {
            return Ok(None);
        }
        if state
            .publication
            .is_some_and(|value| value > self.consumer_release)
        {
            return Ok(None);
        }
        let Some(serial) = self.next_lease.checked_add(1) else {
            self.retiring = Some(RetireReason::CounterExhausted);
            return Err(ControllerError::Retiring(RetireReason::CounterExhausted));
        };
        let slot = ImageSlot::new(index as u32, self.profile.image_count).unwrap();
        let prerequisite = ReusePrerequisite {
            generation: self.binding.generation(),
        };
        match self
            .backend
            .as_mut()
            .unwrap()
            .poll_acquire(slot, &prerequisite, info)
        {
            AcquirePoll::Pending => Ok(None),
            AcquirePoll::Acquired(token) => {
                state.token = Some(token);
                state.serial = serial;
                state.submission = None;
                state.publication = None;
                self.next_lease = serial;
                Ok(Some(AppLease {
                    identity: self.identity.clone(),
                    slot,
                    generation: self.binding.generation(),
                    serial,
                }))
            }
            AcquirePoll::Failed(error) => Err(ControllerError::Backend(error)),
            AcquirePoll::Indeterminate { work, error } => {
                self.uncertain.push(work);
                self.retiring = Some(RetireReason::UncertainOperation);
                Err(ControllerError::Backend(error))
            }
        }
    }

    fn take_token(
        &mut self,
        lease: &AppLease,
    ) -> Result<B::AcquireToken, ControllerError<B::Error>> {
        if !Arc::ptr_eq(&lease.identity, &self.identity)
            || lease.generation != self.binding.generation()
        {
            return Err(ControllerError::Contract(
                "stale application lease generation",
            ));
        }
        let state = self
            .slots
            .get_mut(lease.slot.index() as usize)
            .filter(|state| state.serial == lease.serial)
            .ok_or(ControllerError::Contract("stale application lease"))?;
        state.token.take().ok_or(ControllerError::Contract(
            "application lease is no longer owned",
        ))
    }

    pub fn present(
        &mut self,
        lease: AppLease,
        info: B::PresentInfo,
    ) -> Result<PublicationSignal, ControllerError<B::Error>> {
        self.available()?;
        let published = self.publisher.is_some();
        self.submit(lease, info, published)
    }

    /// A replaced WSI generation can finish its outstanding application leases
    /// even after its viewer connection has been superseded.
    pub fn present_retired(
        &mut self,
        lease: AppLease,
        info: B::PresentInfo,
    ) -> Result<PublicationSignal, ControllerError<B::Error>> {
        self.submit(lease, info, false)
    }

    fn submit(
        &mut self,
        lease: AppLease,
        info: B::PresentInfo,
        published: bool,
    ) -> Result<PublicationSignal, ControllerError<B::Error>> {
        // The consumer reads the newest image slot from the ready timeline
        // itself, so every value carries its slot. Values stay below the
        // terminal release value and the native device-removal sentinel.
        let Some(value) = FrameReady::sequence(self.next_value)
            .checked_add(1)
            .map(|sequence| FrameReady::ready_value(sequence, lease.slot.index()))
            .filter(|value| {
                *value < RELEASE_ALL_SYNC_VALUE
                    && FrameReady::sequence(*value) > FrameReady::sequence(self.next_value)
            })
        else {
            self.retiring = Some(RetireReason::CounterExhausted);
            return Err(ControllerError::Retiring(RetireReason::CounterExhausted));
        };
        let token = self.take_token(&lease)?;
        // Reserve before the driver call. Definite failure also consumes the
        // value; partial success can never cause a lower future signal.
        self.next_value = value;
        let signal = PublicationSignal {
            slot: lease.slot,
            generation: lease.generation,
            value,
        };
        match self
            .backend
            .as_mut()
            .unwrap()
            .submit_present(token, info, signal)
        {
            SubmitOutcome::Submitted(submission) => {
                let state = &mut self.slots[signal.slot.index() as usize];
                state.submission = Some(submission);
                state.producer_complete = false;
                if published {
                    state.publication = Some(signal.value);
                    if FrameReady::sequence(signal.value) == 1
                        || FrameReady::sequence(signal.value).is_multiple_of(120)
                    {
                        tracing::debug!(configured_index = self.binding.configured_index(), logical_id = self.binding.logical_id(),
                            generation = signal.generation, slot = signal.slot.index(), ready_value = signal.value,
                            source = ?self.binding.source(), "polymer: virtual frame published");
                    }
                }
                Ok(signal)
            }
            SubmitOutcome::NotSubmitted { token, error } => {
                self.cancel_token(lease.slot, token)?;
                Err(ControllerError::Backend(error))
            }
            SubmitOutcome::Indeterminate { work, error } => {
                self.uncertain.push(work);
                self.retiring = Some(RetireReason::UncertainOperation);
                Err(ControllerError::Backend(error))
            }
        }
    }

    pub fn cancel(&mut self, lease: AppLease) -> Result<(), ControllerError<B::Error>> {
        let token = self.take_token(&lease)?;
        self.cancel_token(lease.slot, token)
    }
    fn cancel_token(
        &mut self,
        slot: ImageSlot,
        token: B::AcquireToken,
    ) -> Result<(), ControllerError<B::Error>> {
        match self.backend.as_mut().unwrap().cancel_acquire(token) {
            CancelOutcome::Released => Ok(()),
            CancelOutcome::Submitted(submission) => {
                let state = &mut self.slots[slot.index() as usize];
                state.submission = Some(submission);
                state.producer_complete = false;
                Ok(())
            }
            CancelOutcome::Indeterminate { work, error } => {
                self.uncertain.push(work);
                self.retiring = Some(RetireReason::UncertainOperation);
                Err(ControllerError::Backend(error))
            }
        }
    }

    fn retirement_work(&mut self) -> RetirementWork<B> {
        let mut work = RetirementWork {
            submissions: Vec::new(),
            acquired: Vec::new(),
            uncertain: std::mem::take(&mut self.uncertain),
            publications: Vec::new(),
        };
        for (index, state) in self.slots.iter_mut().enumerate() {
            let slot = ImageSlot::new(index as u32, self.profile.image_count).unwrap();
            if let Some(token) = state.token.take() {
                work.acquired.push((slot, token));
            }
            if let Some(submission) = state.submission.take() {
                work.submissions.push((slot, submission));
            }
            if let Some(value) = state.publication {
                work.publications.push((slot, value));
            }
        }
        work
    }
    pub fn retire(mut self, reason: RetireReason) -> BackendRetirement<B> {
        let work = self.retirement_work();
        self.backend
            .take()
            .unwrap()
            .retire(work, self.retiring.unwrap_or(reason))
            .with_permit(self.permit.take().unwrap())
    }
}

impl<B: SwapchainBackend> Drop for SwapchainController<B> {
    fn drop(&mut self) {
        if let Some(backend) = self.backend.take() {
            // Unwinding must never recycle a live API token or submission.
            let work = self.retirement_work();
            std::mem::forget((backend, work, self.permit.take()));
        }
    }
}
