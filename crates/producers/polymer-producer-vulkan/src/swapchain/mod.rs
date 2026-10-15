//! Native virtual WSI images. The hook translates WSI calls and layouts; this
//! backend submits only external-ownership handoffs, never application commands.
pub mod native;
mod ownership;
use crate::{ExportImageRing, ExportImageRingDesc, ExportTimelineSemaphore, ExternalDevice};
use ash::vk;
use native::{Queue, Semaphore, Submission};
use polymer_presentation::{producer::ExportDescriptor, swapchain::*};
use polymer_protocol::{ConsumerApi, ExternalImageKind, GraphicsHook, SurfaceId};
use std::sync::Arc;

pub struct CreateInfo {
    pub external: Arc<ExternalDevice>,
    pub acquire_queue: Arc<Queue>,
    pub queue_families: Vec<u32>,
    pub format: vk::Format,
    pub usage: vk::ImageUsageFlags,
    pub count: u32,
}

#[derive(Default)]
pub struct AcquireInfo {
    pub semaphore: vk::Semaphore,
    pub fence: vk::Fence,
}
pub struct PresentInfo {
    pub queue: Arc<Queue>,
    pub dependency: Arc<Semaphore>,
}
pub struct WriteToken {
    slot: ImageSlot,
    generation: u64,
    acquire: Arc<Submission>,
}
/// The consumer's release timeline, waitable without the controller's owner.
#[derive(Clone)]
pub struct ReleaseTimeline(Arc<ExportTimelineSemaphore>);
impl ReleaseTimeline {
    pub fn wait(&self, value: u64, timeout: u64) -> Result<(), vk::Result> {
        self.0.wait(value, timeout)
    }
}

pub struct VulkanVirtualBackend {
    external: Arc<ExternalDevice>,
    acquire_queue: Arc<Queue>,
    binding: FixedSurfaceBinding,
    ring: ExportImageRing,
    handles: Vec<i64>,
    ready: ExportTimelineSemaphore,
    released: Arc<ExportTimelineSemaphore>,
    initialized: Vec<bool>,
    last_ready: u64,
    concurrent: bool,
    format: u32,
}

impl VulkanVirtualBackend {
    pub fn release_timeline(&self) -> ReleaseTimeline {
        ReleaseTimeline(self.released.clone())
    }
    pub fn format(format: vk::Format) -> Option<u32> {
        match format {
            vk::Format::R8G8B8A8_UNORM | vk::Format::R8G8B8A8_SRGB => Some(28),
            vk::Format::B8G8R8A8_UNORM | vk::Format::B8G8R8A8_SRGB => Some(87),
            vk::Format::A2B10G10R10_UNORM_PACK32 => Some(24),
            vk::Format::R16G16B16A16_SFLOAT => Some(10),
            _ => None,
        }
    }
}

impl SwapchainBackend for VulkanVirtualBackend {
    type CreateInfo = CreateInfo;
    type ApiAccess = vk::Image;
    type AcquireToken = WriteToken;
    type AcquireInfo = AcquireInfo;
    type PresentInfo = PresentInfo;
    type Submission = Arc<Submission>;
    type UncertainWork = Option<WriteToken>;
    type Error = vk::Result;

    fn probe(
        info: &CreateInfo,
        _binding: &FixedSurfaceBinding,
        consumer: ConsumerApi,
    ) -> Result<BackendProfile, vk::Result> {
        if !matches!(consumer, ConsumerApi::D3d12 | ConsumerApi::Vulkan)
            || !(2..=4).contains(&info.count)
        {
            return Err(vk::Result::ERROR_INITIALIZATION_FAILED);
        }
        Ok(BackendProfile {
            image_kind: ExternalImageKind::D3d12Resource,
            format: Self::format(info.format).ok_or(vk::Result::ERROR_FORMAT_NOT_SUPPORTED)?,
            image_count: info.count,
        })
    }

    fn create(
        info: CreateInfo,
        binding: &FixedSurfaceBinding,
        profile: &BackendProfile,
    ) -> Result<Self, vk::Result> {
        let external = &info.external;
        let ring = ExportImageRing::create(
            external,
            ExportImageRingDesc {
                extent: vk::Extent2D {
                    width: binding.extent().width,
                    height: binding.extent().height,
                },
                format: info.format,
                slots: info.count,
                usage: info.usage,
                queue_families: info.queue_families.clone(),
            },
        )?;
        let timelines = (0..2)
            .map(|_| ExportTimelineSemaphore::create(external))
            .collect::<Result<Vec<_>, _>>()?;
        let mut timelines = timelines.into_iter();
        let handles = ring
            .slots()
            .iter()
            .map(|slot| slot.raw_handle() as i64)
            .collect();
        Ok(Self {
            external: info.external,
            acquire_queue: info.acquire_queue,
            binding: binding.clone(),
            ring,
            handles,
            ready: timelines.next().unwrap(),
            released: Arc::new(timelines.next().unwrap()),
            initialized: vec![false; info.count as usize],
            last_ready: 0,
            concurrent: info.queue_families.len() > 1,
            format: profile.format,
        })
    }

    fn export_descriptor(&self) -> ExportDescriptor<'_> {
        ExportDescriptor {
            surface_id: SurfaceId(self.binding.logical_id()),
            adapter_luid: self.external.adapter_luid,
            source_window: self.binding.window().handle as i64,
            hook: GraphicsHook::Vulkan,
            width: self.binding.extent().width,
            height: self.binding.extent().height,
            format: self.format,
            image_kind: ExternalImageKind::D3d12Resource,
            image_handles: &self.handles,
            timeline_semaphore_handle: self.ready.raw_handle() as i64,
            consumer_semaphore_handle: self.released.raw_handle() as i64,
            bottom_up: false,
        }
    }
    fn api_access(&self, slot: ImageSlot) -> vk::Image {
        self.ring.slots()[slot.index() as usize].image()
    }

    fn poll_acquire(
        &mut self,
        slot: ImageSlot,
        prerequisite: &ReusePrerequisite,
        info: &AcquireInfo,
    ) -> AcquirePoll<WriteToken, Option<WriteToken>, vk::Result> {
        let barrier = self.acquire_barrier(slot);
        let signals = (info.semaphore != vk::Semaphore::null())
            .then_some((info.semaphore, 0))
            .into_iter()
            .collect::<Vec<_>>();
        match self
            .acquire_queue
            .submit(&[barrier], &[], &signals, info.fence, Vec::new())
        {
            Ok(acquire) => {
                self.initialized[slot.index() as usize] = true;
                AcquirePoll::Acquired(WriteToken {
                    slot,
                    generation: prerequisite.generation,
                    acquire,
                })
            }
            Err(vk::Result::ERROR_DEVICE_LOST) => AcquirePoll::Indeterminate {
                work: None,
                error: vk::Result::ERROR_DEVICE_LOST,
            },
            Err(error) => AcquirePoll::Failed(error),
        }
    }

    fn submit_present(
        &mut self,
        token: WriteToken,
        info: PresentInfo,
        signal: PublicationSignal,
    ) -> SubmitOutcome<Arc<Submission>, WriteToken, Option<WriteToken>, vk::Result> {
        if token.slot != signal.slot() || token.generation != self.binding.generation() {
            return SubmitOutcome::NotSubmitted {
                token,
                error: vk::Result::ERROR_OUT_OF_DATE_KHR,
            };
        }
        let barrier = self.release_barrier(token.slot, info.queue.family);
        let mut waits = vec![(info.dependency.handle, 0)];
        if self.last_ready != 0 {
            waits.push((self.ready.semaphore(), self.last_ready));
        }
        match info.queue.submit(
            &[barrier],
            &waits,
            &[(self.ready.semaphore(), signal.value())],
            vk::Fence::null(),
            vec![info.dependency],
        ) {
            Ok(work) => {
                self.last_ready = signal.value();
                SubmitOutcome::Submitted(work)
            }
            Err(error)
                if matches!(
                    error,
                    vk::Result::ERROR_OUT_OF_HOST_MEMORY | vk::Result::ERROR_OUT_OF_DEVICE_MEMORY
                ) =>
            {
                SubmitOutcome::NotSubmitted { token, error }
            }
            Err(error) => SubmitOutcome::Indeterminate {
                work: Some(token),
                error,
            },
        }
    }
    fn cancel_acquire(
        &mut self,
        token: WriteToken,
    ) -> CancelOutcome<Arc<Submission>, Option<WriteToken>, vk::Result> {
        CancelOutcome::Submitted(token.acquire)
    }
    fn poll_progress(
        &mut self,
        submissions: &[SubmissionRef<'_, Arc<Submission>>],
    ) -> Result<BackendProgress, vk::Result> {
        let mut completed = Vec::new();
        for submission in submissions {
            if submission.submission.complete()? {
                completed.push(submission.slot);
            }
        }
        let released = unsafe {
            self.external
                .device
                .get_semaphore_counter_value(self.released.semaphore())?
        };
        Ok(BackendProgress {
            completed,
            released,
            device_lost: false,
        })
    }
    fn poll_retirement(
        &mut self,
        work: &mut RetirementWork<Self>,
        _reason: RetireReason,
    ) -> RetirementProgress {
        self.retirement(work)
    }
}
