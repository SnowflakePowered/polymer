//! Native fixed-ring backend. Present returns only after acquiring the next
//! buffer. Applications submit in PRESENT/COMMON on the presentation queue.
pub mod creation;
pub mod frontend;
mod resources;
use polymer_presentation::{producer::ExportDescriptor, swapchain::*};
use polymer_protocol::{AdapterLuid, ConsumerApi, ExternalImageKind, GraphicsHook, SurfaceId};
use resources::{SharedFence, SharedImage};
use windows::{
    Win32::{
        Foundation::{E_INVALIDARG, E_POINTER},
        Graphics::{
            Direct3D12::*,
            Dxgi::Common::{
                DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM,
                DXGI_FORMAT_R10G10B10A2_UNORM,
            },
        },
    },
    core::{Error, Interface},
};

pub struct CreateInfo {
    pub device: ID3D12Device,
    pub queue: ID3D12CommandQueue,
    pub format: DXGI_FORMAT,
    pub image_count: u32,
}

pub struct WriteToken {
    slot: ImageSlot,
    generation: u64,
}

pub struct Submission {
    fence: ID3D12Fence,
    value: u64,
}

pub struct D3d12VirtualBackend {
    device: ID3D12Device,
    queue: ID3D12CommandQueue,
    binding: FixedSurfaceBinding,
    profile: BackendProfile,
    images: Vec<SharedImage>,
    handles: Vec<i64>,
    ready: SharedFence,
    released: SharedFence,
    cleanup: ID3D12Fence,
    cleanup_value: u64,
}

impl D3d12VirtualBackend {
    fn arm_release(
        &self,
        value: u64,
        event: windows::Win32::Foundation::HANDLE,
    ) -> Result<(), Error> {
        unsafe { self.released.fence.SetEventOnCompletion(value, event) }
    }
}

impl SwapchainBackend for D3d12VirtualBackend {
    type CreateInfo = CreateInfo;
    type ApiAccess = ID3D12Resource;
    type AcquireToken = WriteToken;
    type AcquireInfo = ();
    type PresentInfo = ();
    type Submission = Submission;
    type UncertainWork = WriteToken;
    type Error = Error;

    fn probe(
        info: &CreateInfo,
        binding: &FixedSurfaceBinding,
        consumer: ConsumerApi,
    ) -> Result<BackendProfile, Error> {
        if !matches!(consumer, ConsumerApi::D3d12 | ConsumerApi::Vulkan) {
            return Err(Error::new(
                E_INVALIDARG,
                "virtual D3D12 requires an explicit existing consumer preference",
            ));
        }
        if !(2..=4).contains(&info.image_count) {
            return Err(Error::new(
                E_INVALIDARG,
                "virtual D3D12 requires two to four application buffers",
            ));
        }
        let desc = unsafe { info.queue.GetDesc() };
        if desc.Type != D3D12_COMMAND_LIST_TYPE_DIRECT || unsafe { info.device.GetNodeCount() } != 1
        {
            return Err(Error::new(
                E_INVALIDARG,
                "virtual D3D12 requires a direct queue on a single-node device",
            ));
        }
        let mut queue_device: Option<ID3D12Device> = None;
        unsafe {
            info.queue.GetDevice(&mut queue_device)?;
        }
        if queue_device
            .ok_or_else(|| Error::from_hresult(E_POINTER))?
            .cast::<windows::core::IUnknown>()?
            != info.device.cast::<windows::core::IUnknown>()?
        {
            return Err(Error::new(
                E_INVALIDARG,
                "virtual swapchain queue belongs to another device",
            ));
        }
        let extent = binding.extent();
        if extent.width > D3D12_REQ_TEXTURE2D_U_OR_V_DIMENSION
            || extent.height > D3D12_REQ_TEXTURE2D_U_OR_V_DIMENSION
        {
            return Err(Error::new(
                E_INVALIDARG,
                "fixed virtual resolution exceeds D3D12 texture limits",
            ));
        }
        Self::check_format(&info.device, info.format)?;
        let profile = BackendProfile {
            image_kind: ExternalImageKind::D3d12Resource,
            format: info.format.0 as u32,
            image_count: info.image_count,
        };
        profile
            .validate()
            .map_err(|message| Error::new(E_INVALIDARG, message))?;
        Ok(profile)
    }

    fn create(
        info: CreateInfo,
        binding: &FixedSurfaceBinding,
        profile: &BackendProfile,
    ) -> Result<Self, Error> {
        let ready = SharedFence::new(&info.device)?;
        let released = SharedFence::new(&info.device)?;
        let cleanup = unsafe { info.device.CreateFence(0, D3D12_FENCE_FLAG_NONE)? };
        let images = (0..profile.image_count)
            .map(|_| SharedImage::new(&info.device, binding.extent(), info.format))
            .collect::<Result<Vec<_>, _>>()?;
        let handles = images.iter().map(SharedImage::handle).collect();
        Ok(Self {
            device: info.device,
            queue: info.queue,
            binding: binding.clone(),
            profile: profile.clone(),
            images,
            handles,
            ready,
            released,
            cleanup,
            cleanup_value: 0,
        })
    }
    fn export_descriptor(&self) -> ExportDescriptor<'_> {
        let luid = unsafe { self.device.GetAdapterLuid() };
        let extent = self.binding.extent();
        ExportDescriptor {
            surface_id: SurfaceId(self.binding.logical_id()),
            adapter_luid: AdapterLuid {
                low_part: luid.LowPart,
                high_part: luid.HighPart,
            },
            source_window: self.binding.window().handle as i64,
            hook: GraphicsHook::D3d12,
            width: extent.width,
            height: extent.height,
            format: self.profile.format,
            image_kind: self.profile.image_kind,
            image_handles: &self.handles,
            timeline_semaphore_handle: self.ready.handle(),
            consumer_semaphore_handle: self.released.handle(),
            bottom_up: false,
        }
    }
    fn api_access(&self, slot: ImageSlot) -> ID3D12Resource {
        self.images[slot.index() as usize].resource.clone()
    }
    fn poll_acquire(
        &mut self,
        slot: ImageSlot,
        prerequisite: &ReusePrerequisite,
        _info: &(),
    ) -> AcquirePoll<WriteToken, WriteToken, Error> {
        let token = WriteToken {
            slot,
            generation: prerequisite.generation,
        };
        AcquirePoll::Acquired(token)
    }
    fn submit_present(
        &mut self,
        token: WriteToken,
        _: (),
        signal: PublicationSignal,
    ) -> SubmitOutcome<Submission, WriteToken, WriteToken, Error> {
        if token.generation != self.binding.generation() || token.slot != signal.slot() {
            return SubmitOutcome::NotSubmitted {
                token,
                error: Error::new(E_INVALIDARG, "stale D3D12 write token"),
            };
        }
        match unsafe { self.queue.Signal(&self.ready.fence, signal.value()) } {
            Ok(()) => SubmitOutcome::Submitted(Submission {
                fence: self.ready.fence.clone(),
                value: signal.value(),
            }),
            Err(error) => SubmitOutcome::Indeterminate { work: token, error },
        }
    }
    fn cancel_acquire(
        &mut self,
        token: WriteToken,
    ) -> CancelOutcome<Submission, WriteToken, Error> {
        let Some(value) = self
            .cleanup_value
            .checked_add(1)
            .filter(|value| *value < u64::MAX)
        else {
            return CancelOutcome::Indeterminate {
                work: token,
                error: Error::new(E_INVALIDARG, "cleanup fence exhausted"),
            };
        };
        self.cleanup_value = value;
        match unsafe { self.queue.Signal(&self.cleanup, value) } {
            Ok(()) => CancelOutcome::Submitted(Submission {
                fence: self.cleanup.clone(),
                value,
            }),
            Err(error) => CancelOutcome::Indeterminate { work: token, error },
        }
    }
    fn poll_progress(
        &mut self,
        submissions: &[SubmissionRef<'_, Submission>],
    ) -> Result<BackendProgress, Error> {
        let released = unsafe { self.released.fence.GetCompletedValue() };
        let mut progress = BackendProgress {
            device_lost: released == u64::MAX
                || unsafe { self.device.GetDeviceRemovedReason() }.is_err(),
            released,
            ..Default::default()
        };
        for submission in submissions {
            let completed = unsafe { submission.submission.fence.GetCompletedValue() };
            if completed == u64::MAX {
                progress.device_lost = true;
            } else if completed >= submission.submission.value {
                progress.completed.push(submission.slot);
            }
        }
        Ok(progress)
    }
    fn arm_progress(
        &self,
        release: Option<u64>,
        submission: Option<&Submission>,
        event: windows::Win32::Foundation::HANDLE,
    ) -> Result<bool, Error> {
        if let Some(value) = release {
            self.arm_release(value, event)?;
        }
        if let Some(submission) = submission {
            unsafe {
                submission
                    .fence
                    .SetEventOnCompletion(submission.value, event)?
            };
        }
        Ok(release.is_some() || submission.is_some())
    }
    fn poll_retirement(
        &mut self,
        work: &mut RetirementWork<Self>,
        _: RetireReason,
    ) -> RetirementProgress {
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
        let producer_done = work.submissions.iter().all(|(_, submission)| {
            let value = unsafe { submission.fence.GetCompletedValue() };
            value != u64::MAX && value >= submission.value
        });
        let release = unsafe { self.released.fence.GetCompletedValue() };
        if producer_done
            && release != u64::MAX
            && work.publications.iter().all(|(_, value)| release >= *value)
        {
            RetirementProgress::Safe
        } else {
            RetirementProgress::Pending
        }
    }
}

impl D3d12VirtualBackend {
    fn check_format(device: &ID3D12Device, format: DXGI_FORMAT) -> Result<(), Error> {
        if !matches!(
            format,
            DXGI_FORMAT_R8G8B8A8_UNORM | DXGI_FORMAT_B8G8R8A8_UNORM | DXGI_FORMAT_R10G10B10A2_UNORM
        ) {
            return Err(Error::new(
                E_INVALIDARG,
                "virtual D3D12 supports RGBA8/BGRA8/RGB10A2 UNORM",
            ));
        }
        let mut support = D3D12_FEATURE_DATA_FORMAT_SUPPORT {
            Format: format,
            ..Default::default()
        };
        unsafe {
            device.CheckFeatureSupport(
                D3D12_FEATURE_FORMAT_SUPPORT,
                (&mut support as *mut D3D12_FEATURE_DATA_FORMAT_SUPPORT).cast(),
                std::mem::size_of_val(&support) as u32,
            )?;
        }
        let required = D3D12_FORMAT_SUPPORT1_TEXTURE2D
            | D3D12_FORMAT_SUPPORT1_RENDER_TARGET
            | D3D12_FORMAT_SUPPORT1_SHADER_SAMPLE;
        if !support.Support1.contains(required) {
            return Err(Error::new(
                E_INVALIDARG,
                "fixed virtual format lacks render/sample support",
            ));
        }
        Ok(())
    }
}
