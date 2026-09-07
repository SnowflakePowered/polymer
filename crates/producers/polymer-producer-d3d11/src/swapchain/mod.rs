//! Native D3D11 buffers with driver-supported allocation identity rotation.
pub mod creation;
pub mod frontend;
mod ownership;
pub mod rotation;
use crate::{D3d11ImageRing, D3d11RingMode};
use polymer_presentation::{producer::ExportDescriptor, swapchain::*};
use polymer_protocol::{AdapterLuid, ConsumerApi, ExternalImageKind, ExternalSyncKind, SurfaceId};
use windows::{
    Win32::{
        Foundation::*,
        Graphics::{
            Direct3D11::*,
            Dxgi::{Common::*, *},
        },
    },
    core::{Error, Interface},
};

pub struct CreateInfo {
    pub device: ID3D11Device,
    pub format: DXGI_FORMAT,
    pub image_count: u32,
}
impl CreateInfo {
    fn validate_threading(&self) -> Result<(), Error> {
        if unsafe { self.device.GetCreationFlags() } & D3D11_CREATE_DEVICE_SINGLETHREADED.0 as u32
            != 0
        {
            return Err(Error::new(
                DXGI_ERROR_UNSUPPORTED,
                "virtual D3D11 currently requires a device permitting serialized cross-thread ownership",
            ));
        }
        Ok(())
    }
}

/// Controller-owned proof of native write access, never released by Drop.
pub struct WriteToken {
    slot: ImageSlot,
    generation: u64,
}

pub struct Submission {
    fence: ID3D11Fence,
    value: u64,
}

/// Driven through exclusive immediate-context ownership. Native context calls
/// may move between threads, but cannot overlap application context calls.
pub struct D3d11VirtualBackend {
    device: ID3D11Device,
    context: ID3D11DeviceContext4,
    gate: ID3D11Multithread,
    binding: FixedSurfaceBinding,
    profile: BackendProfile,
    ring: D3d11ImageRing,
    rotation: Box<dyn rotation::ResourceRotation>,
    allocations: Vec<u32>,
    handles: Vec<i64>,
    adapter_luid: AdapterLuid,
    cleanup: ID3D11Fence,
    cleanup_value: u64,
}

impl SwapchainBackend for D3d11VirtualBackend {
    type CreateInfo = CreateInfo;
    type ApiAccess = ID3D11Texture2D;
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
        info.validate_threading()?;
        if !(2..=4).contains(&info.image_count) {
            return Err(Error::new(
                E_INVALIDARG,
                "virtual D3D11 requires two to four shared images",
            ));
        }
        if !matches!(consumer, ConsumerApi::D3d12 | ConsumerApi::Vulkan) {
            return Err(Error::new(
                DXGI_ERROR_UNSUPPORTED,
                "virtual D3D11 requires an explicit consumer preference",
            ));
        }
        let image_kind = ExternalImageKind::D3d11D3d12Resource;
        let sync_kind = ExternalSyncKind::D3d12Fence;
        let _: ID3D11Device5 = info.device.cast()?;
        let _: ID3D11DeviceContext4 = unsafe { info.device.GetImmediateContext()? }.cast()?;
        let extent = binding.extent();
        if extent.width > D3D11_REQ_TEXTURE2D_U_OR_V_DIMENSION
            || extent.height > D3D11_REQ_TEXTURE2D_U_OR_V_DIMENSION
        {
            return Err(Error::new(
                E_INVALIDARG,
                "fixed virtual resolution exceeds D3D11 texture limits",
            ));
        }
        if !matches!(
            info.format,
            DXGI_FORMAT_R8G8B8A8_UNORM | DXGI_FORMAT_B8G8R8A8_UNORM | DXGI_FORMAT_R10G10B10A2_UNORM
        ) {
            return Err(Error::new(
                DXGI_ERROR_UNSUPPORTED,
                "virtual D3D11 requires RGBA8, BGRA8, or RGB10A2 UNORM",
            ));
        }
        let support = unsafe { info.device.CheckFormatSupport(info.format)? };
        let required = (D3D11_FORMAT_SUPPORT_TEXTURE2D.0
            | D3D11_FORMAT_SUPPORT_RENDER_TARGET.0
            | D3D11_FORMAT_SUPPORT_SHADER_SAMPLE.0) as u32;
        if support & required != required {
            return Err(Error::new(
                DXGI_ERROR_UNSUPPORTED,
                "virtual D3D11 format lacks render/sample support",
            ));
        }
        Ok(BackendProfile {
            image_kind,
            sync_kind,
            format: info.format.0 as u32,
            image_count: info.image_count,
        })
    }

    fn create(
        info: CreateInfo,
        binding: &FixedSurfaceBinding,
        profile: &BackendProfile,
    ) -> Result<Self, Error> {
        info.validate_threading()?;
        let context: ID3D11DeviceContext4 = unsafe { info.device.GetImmediateContext()? }.cast()?;
        let gate: ID3D11Multithread = context.cast()?;
        unsafe {
            let _ = gate.SetMultithreadProtected(true);
        }
        let adapter = unsafe { info.device.cast::<IDXGIDevice>()?.GetAdapter()? };
        let luid = unsafe { adapter.GetDesc()? }.AdapterLuid;
        let extent = binding.extent();
        let description = D3D11_TEXTURE2D_DESC {
            Width: extent.width,
            Height: extent.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: info.format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE).0 as u32,
            ..Default::default()
        };
        let capture = rotation::RotationHooks::begin()?;
        let ring = unsafe {
            D3d11ImageRing::new(
                &info.device,
                &description,
                profile.image_count,
                D3d11RingMode::Fence,
            )
        }?;
        let rotation = capture.finish(
            &ring
                .slots()
                .iter()
                .map(|slot| slot.texture.clone())
                .collect::<Vec<_>>(),
        )?;
        let handles = ring
            .slots()
            .iter()
            .map(|slot| slot.raw_handle() as i64)
            .collect();
        let mut cleanup = None;
        unsafe {
            info.device.cast::<ID3D11Device5>()?.CreateFence(
                0,
                D3D11_FENCE_FLAG_NONE,
                &mut cleanup,
            )?;
        }
        Ok(Self {
            device: info.device,
            context,
            gate,
            binding: binding.clone(),
            profile: profile.clone(),
            ring,
            rotation,
            allocations: (0..profile.image_count).collect(),
            handles,
            adapter_luid: AdapterLuid {
                low_part: luid.LowPart,
                high_part: luid.HighPart,
            },
            cleanup: cleanup.ok_or_else(|| Error::from_hresult(E_POINTER))?,
            cleanup_value: 0,
        })
    }

    fn export_descriptor(&self) -> ExportDescriptor<'_> {
        let extent = self.binding.extent();
        ExportDescriptor {
            surface_id: SurfaceId(self.binding.logical_id()),
            adapter_luid: self.adapter_luid,
            source_window: self.binding.window().handle as i64,
            width: extent.width,
            height: extent.height,
            format: self.profile.format,
            image_kind: self.profile.image_kind,
            sync_kind: self.profile.sync_kind,
            image_handles: &self.handles,
            timeline_semaphore_handle: self.ring.raw_fence_handle().expect("fence profile") as i64,
            consumer_semaphore_handle: self
                .ring
                .raw_consumer_fence_handle()
                .map_or(0, |handle| handle as i64),
            bottom_up: false,
        }
    }
    fn api_access(&self, _slot: ImageSlot) -> ID3D11Texture2D {
        self.ring.slots()[0].texture.clone()
    }
    fn poll_acquire(
        &mut self,
        slot: ImageSlot,
        prerequisite: &ReusePrerequisite,
        _info: &(),
    ) -> AcquirePoll<WriteToken, WriteToken, Error> {
        let _access = ownership::ImmediateAccess::enter(&self.gate);
        let token = WriteToken {
            slot,
            generation: prerequisite.generation,
        };
        let fence = self.ring.consumer_fence().expect("shared release fence");
        let completed = unsafe { fence.GetCompletedValue() };
        if completed == u64::MAX {
            return AcquirePoll::Failed(Error::from_hresult(DXGI_ERROR_DEVICE_REMOVED));
        }
        let value = prerequisite.previous_publication.unwrap_or(0);
        if completed < value {
            return AcquirePoll::Pending;
        }
        if value != 0 {
            if let Err(error) = unsafe { self.context.Wait(fence, value) } {
                return AcquirePoll::Indeterminate { work: token, error };
            }
        }
        let native = self
            .allocations
            .iter()
            .position(|allocation| *allocation == slot.index())
            .expect("physical slot");
        if native != 0 {
            // Drain the runtime's immediate batch before changing driver identities.
            unsafe {
                self.context.Flush();
            }
            if let Err(error) = unsafe { self.rotation.exchange(0, native) } {
                return AcquirePoll::Indeterminate { work: token, error };
            }
            self.allocations.swap(0, native);
        }
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
                error: Error::new(E_INVALIDARG, "stale D3D11 write token"),
            };
        }
        let fence = self.ring.fence().expect("fence profile").clone();
        match self.release(token, &fence, signal.value()) {
            Ok(submission) => SubmitOutcome::Submitted(submission),
            Err((work, error)) => SubmitOutcome::Indeterminate { work, error },
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
                error: Error::new(E_FAIL, "D3D11 cleanup fence exhausted"),
            };
        };
        self.cleanup_value = value;
        match self.release(token, &self.cleanup, value) {
            Ok(submission) => CancelOutcome::Submitted(submission),
            Err((work, error)) => CancelOutcome::Indeterminate { work, error },
        }
    }
    fn poll_progress(
        &mut self,
        submissions: &[SubmissionRef<'_, Submission>],
    ) -> Result<BackendProgress, Error> {
        let mut progress = BackendProgress {
            device_lost: unsafe { self.device.GetDeviceRemovedReason() }.is_err(),
            ..Default::default()
        };
        for submission in submissions {
            let value = unsafe { submission.submission.fence.GetCompletedValue() };
            if value == u64::MAX {
                progress.device_lost = true;
            } else if value >= submission.submission.value {
                progress.completed.push(submission.slot);
            }
        }
        Ok(progress)
    }
    fn poll_retirement(
        &mut self,
        work: &mut RetirementWork<Self>,
        _: RetireReason,
    ) -> RetirementProgress {
        self.retirement_progress(work)
    }
}
