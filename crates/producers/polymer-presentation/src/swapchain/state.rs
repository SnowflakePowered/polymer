use super::SwapchainBackend;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ImageSlot(u32);
impl ImageSlot {
    pub fn new(index: u32, image_count: u32) -> Option<Self> {
        (index < image_count && image_count as usize <= polymer_protocol::MAX_IMAGE_SLOTS)
            .then_some(Self(index))
    }
    pub const fn index(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ReusePrerequisite {
    pub generation: u64,
    pub previous_publication: Option<u64>,
}

/// Only the controller can reserve a timeline value.
#[derive(Clone, Copy, Debug)]
pub struct PublicationSignal {
    pub(crate) slot: ImageSlot,
    pub(crate) generation: u64,
    pub(crate) value: u64,
}
impl PublicationSignal {
    pub fn slot(self) -> ImageSlot {
        self.slot
    }
    pub fn value(self) -> u64 {
        self.value
    }
}

pub enum AcquirePoll<T, U, E> {
    Pending,
    Acquired(T),
    Failed(E),
    Indeterminate { work: U, error: E },
}
pub enum SubmitOutcome<S, T, U, E> {
    Submitted(S),
    NotSubmitted { token: T, error: E },
    Indeterminate { work: U, error: E },
}
pub enum CancelOutcome<S, U, E> {
    Released,
    Submitted(S),
    Indeterminate { work: U, error: E },
}

pub struct SubmissionRef<'a, S> {
    pub slot: ImageSlot,
    pub submission: &'a S,
}

#[derive(Default)]
pub struct BackendProgress {
    pub completed: Vec<ImageSlot>,
    pub device_lost: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetireReason {
    ApplicationDestroyed,
    ConnectionLost,
    DeviceLost,
    UncertainOperation,
    CounterExhausted,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetirementProgress {
    Pending,
    Safe,
    Quarantined,
}

pub struct RetirementWork<B: SwapchainBackend> {
    pub submissions: Vec<(ImageSlot, B::Submission)>,
    pub acquired: Vec<(ImageSlot, B::AcquireToken)>,
    pub uncertain: Vec<B::UncertainWork>,
    /// Includes possibly observed IPC sends; disconnect never clears these.
    pub publications: Vec<(ImageSlot, u64)>,
}

/// The API frontend retains this lease while the guest owns an image. Dropping
/// it does not release the backend token: the controller retains that token.
#[derive(Debug)]
pub struct AppLease {
    pub(crate) identity: std::sync::Arc<()>,
    pub(crate) slot: ImageSlot,
    pub(crate) generation: u64,
    pub(crate) serial: u64,
}
impl AppLease {
    pub fn slot(&self) -> ImageSlot {
        self.slot
    }
}

pub(crate) struct SlotState<B: SwapchainBackend> {
    pub producer_complete: bool,
    pub token: Option<B::AcquireToken>,
    pub serial: u64,
    pub submission: Option<B::Submission>,
    pub publication: Option<u64>,
}
impl<B: SwapchainBackend> Default for SlotState<B> {
    fn default() -> Self {
        Self {
            token: None,
            serial: 0,
            submission: None,
            producer_complete: true,
            publication: None,
        }
    }
}
