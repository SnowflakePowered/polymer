mod resources;
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_metal::*;
use polymer_ipc::xpc::MetalResources;
use polymer_presentation::{producer::ExportDescriptor, swapchain::*};
use polymer_protocol::{AdapterLuid, ConsumerApi, ExternalImageKind, GraphicsHook, SurfaceId};
use resources::Image;
use std::{io, sync::Arc};

pub struct CreateInfo {
    pub device: Retained<ProtocolObject<dyn MTLDevice>>,
    pub format: MTLPixelFormat,
    pub image_count: u32,
}
pub struct TextureAccess {
    pub texture: Retained<ProtocolObject<dyn MTLTexture>>,
    _lease: Arc<()>,
}
// SAFETY: Metal resources are thread-safe; mutable controller access serializes ownership.
unsafe impl Send for TextureAccess {}
pub struct MetalVirtualBackend {
    binding: FixedSurfaceBinding,
    profile: BackendProfile,
    images: Vec<Image>,
    references: Vec<Arc<()>>,
    ready: Retained<ProtocolObject<dyn MTLSharedEvent>>,
    released: Retained<ProtocolObject<dyn MTLSharedEvent>>,
    handles: Vec<i64>,
}
unsafe impl Send for MetalVirtualBackend {}
/// Submitted guest work captured by the hook before a virtual present.
/// The owner worker waits for completion before publishing, never a driver callback.
pub struct PresentWork(pub Vec<Retained<ProtocolObject<dyn MTLCommandBuffer>>>);
unsafe impl Send for PresentWork {}

impl SwapchainBackend for MetalVirtualBackend {
    type CreateInfo = CreateInfo;
    type ApiAccess = TextureAccess;
    type AcquireToken = ImageSlot;
    type AcquireInfo = ();
    type PresentInfo = PresentWork;
    type Submission = u64;
    type UncertainWork = PresentWork;
    type Error = io::Error;
    fn probe(
        info: &CreateInfo,
        _: &FixedSurfaceBinding,
        consumer: ConsumerApi,
    ) -> io::Result<BackendProfile> {
        if consumer != ConsumerApi::Metal || !(2..=3).contains(&info.image_count) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Metal requires its native consumer and two or three images",
            ));
        }
        Ok(BackendProfile {
            image_kind: ExternalImageKind::IOSurface,
            format: info.format.0 as u32,
            image_count: info.image_count,
        })
    }
    fn create(
        info: CreateInfo,
        binding: &FixedSurfaceBinding,
        profile: &BackendProfile,
    ) -> io::Result<Self> {
        let images = (0..profile.image_count)
            .map(|_| Image::create(&info.device, binding.extent(), info.format))
            .collect::<io::Result<Vec<_>>>()?;
        Ok(Self {
            binding: binding.clone(),
            profile: profile.clone(),
            images,
            references: (0..profile.image_count).map(|_| Arc::new(())).collect(),
            ready: info
                .device
                .newSharedEvent()
                .ok_or_else(|| io::Error::other("Metal shared event unavailable"))?,
            released: info
                .device
                .newSharedEvent()
                .ok_or_else(|| io::Error::other("Metal shared event unavailable"))?,
            handles: vec![0; profile.image_count as usize],
        })
    }
    fn export_descriptor(&self) -> ExportDescriptor<'_> {
        ExportDescriptor {
            resources: MetalResources {
                surfaces: self.images.iter().map(|v| v.surface.clone()).collect(),
                ready: self.ready.newSharedEventHandle(),
                released: self.released.newSharedEventHandle(),
            },
            surface_id: SurfaceId(self.binding.logical_id()),
            adapter_luid: AdapterLuid {
                low_part: 0,
                high_part: 0,
            },
            source_window: self.binding.window().handle as i64,
            hook: GraphicsHook::Metal,
            width: self.binding.extent().width,
            height: self.binding.extent().height,
            format: self.profile.format,
            image_kind: self.profile.image_kind,
            image_handles: &self.handles,
            timeline_semaphore_handle: 0,
            consumer_semaphore_handle: 0,
            bottom_up: false,
        }
    }
    fn api_access(&self, slot: ImageSlot) -> TextureAccess {
        TextureAccess {
            texture: self.images[slot.index() as usize].texture.clone(),
            _lease: self.references[slot.index() as usize].clone(),
        }
    }
    fn poll_acquire(
        &mut self,
        slot: ImageSlot,
        prerequisite: &ReusePrerequisite,
        _: &(),
    ) -> AcquirePoll<ImageSlot, PresentWork, io::Error> {
        if Arc::strong_count(&self.references[slot.index() as usize]) != 1 {
            AcquirePoll::Pending
        } else {
            AcquirePoll::Acquired(slot)
        }
    }
    fn submit_present(
        &mut self,
        _: ImageSlot,
        info: PresentWork,
        signal: PublicationSignal,
    ) -> SubmitOutcome<u64, ImageSlot, PresentWork, io::Error> {
        for buffer in &info.0 {
            buffer.waitUntilCompleted();
            if buffer.status() == MTLCommandBufferStatus::Error {
                return SubmitOutcome::Indeterminate {
                    work: info,
                    error: io::Error::other("guest Metal command buffer failed"),
                };
            }
        }
        self.ready.setSignaledValue(signal.value());
        SubmitOutcome::Submitted(signal.value())
    }
    fn cancel_acquire(&mut self, _: ImageSlot) -> CancelOutcome<u64, PresentWork, io::Error> {
        CancelOutcome::Released
    }
    fn poll_progress(
        &mut self,
        submissions: &[SubmissionRef<'_, u64>],
    ) -> io::Result<BackendProgress> {
        Ok(BackendProgress {
            completed: submissions
                .iter()
                .filter(|s| *s.submission <= self.ready.signaledValue())
                .map(|s| s.slot)
                .collect(),
            released: self.released.signaledValue(),
            device_lost: false,
        })
    }
    fn poll_retirement(
        &mut self,
        work: &mut RetirementWork<Self>,
        _: RetireReason,
    ) -> RetirementProgress {
        if !work.uncertain.is_empty() {
            return RetirementProgress::Quarantined;
        }
        if self.references.iter().any(|v| Arc::strong_count(v) != 1)
            || work
                .publications
                .iter()
                .any(|(_, value)| self.released.signaledValue() < *value)
        {
            RetirementProgress::Pending
        } else {
            RetirementProgress::Safe
        }
    }
}
