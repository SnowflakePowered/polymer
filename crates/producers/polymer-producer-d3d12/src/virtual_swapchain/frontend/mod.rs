mod pacing;
mod presentation;
mod retirement;

use super::D3d12VirtualBackend;
use polymer_hook_dxgi::virtual_swapchain::VirtualDxgiDriver;
use polymer_presentation::swapchain::*;
use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};
use windows::{
    Win32::{
        Foundation::*,
        Graphics::{Direct3D12::*, Dxgi::*},
    },
    core::{Error, HRESULT, IUnknown, Interface},
};

struct State {
    controller: SwapchainController<D3d12VirtualBackend>,
    leases: Vec<Option<AppLease>>,
    failed: bool,
}

/// Native indexed buffers. Present acquires the next lease before returning.
pub struct D3d12Frontend {
    state: Option<OwnerCell<State>>,
    resources: Vec<ID3D12Resource>,
    current: AtomicU32,
    registration: Option<SourceRegistration>,
    fixed_window: Option<polymer_platform_windows::FixedWindowBinding>,
    pacing: Option<FrameLatency>,
}

impl D3d12Frontend {
    pub fn new(
        mut controller: SwapchainController<D3d12VirtualBackend>,
    ) -> windows::core::Result<Self> {
        let count = controller.profile().image_count;
        let resources = (0..count)
            .map(|index| {
                controller
                    .api_access(ImageSlot::new(index, count).unwrap())
                    .map_err(Self::error)
            })
            .collect::<windows::core::Result<Vec<_>>>()?;
        let initial = controller
            .acquire(
                Some(ImageSlot::new(0, count).unwrap()),
                AcquireDeadline::after(Duration::from_secs(2)),
            )
            .map_err(Self::error)?;
        let mut leases = (0..count).map(|_| None).collect::<Vec<_>>();
        leases[0] = Some(initial);
        Ok(Self {
            state: Some(OwnerCell::new(State {
                controller,
                leases,
                failed: false,
            })),
            resources,
            current: AtomicU32::new(0),
            registration: None,
            fixed_window: None,
            pacing: None,
        })
    }
    pub(super) fn bind(
        mut self,
        registration: SourceRegistration,
        fixed_window: polymer_platform_windows::FixedWindowBinding,
    ) -> Self {
        self.registration = Some(registration);
        self.fixed_window = Some(fixed_window);
        self
    }

    pub fn retire(mut self) -> FrontendRetirement {
        self.take_retirement().expect("frontend not in a call")
    }

    fn error(error: ControllerError<Error>) -> Error {
        match error {
            ControllerError::Backend(error) => error,
            ControllerError::WouldBlock | ControllerError::Timeout => {
                Error::from_hresult(DXGI_ERROR_WAS_STILL_DRAWING)
            }
            error => Error::new(
                DXGI_ERROR_DEVICE_REMOVED,
                format!("virtual D3D12: {error:?}"),
            ),
        }
    }
    fn owner_error(error: OwnerUnavailable) -> Error {
        Error::new(
            match error {
                OwnerUnavailable::Busy => DXGI_ERROR_WAS_STILL_DRAWING,
                OwnerUnavailable::Reentrant => DXGI_ERROR_INVALID_CALL,
                OwnerUnavailable::Abandoned => DXGI_ERROR_DEVICE_REMOVED,
            },
            format!("virtual D3D12 owner unavailable: {error:?}"),
        )
    }
}

pub type FrontendRetirement = BackendRetirement<D3d12VirtualBackend>;

impl State {
    fn acquire(
        &mut self,
        index: usize,
        deadline: AcquireDeadline,
        maximum: Option<u32>,
    ) -> windows::core::Result<()> {
        if self.failed {
            return Err(Error::from_hresult(DXGI_ERROR_DEVICE_REMOVED));
        }
        if self.leases[index].is_some() {
            if let Some(maximum) = maximum {
                self.controller
                    .wait_for_pacing(maximum, deadline)
                    .map_err(D3d12Frontend::error)?;
            }
        }
        if self.leases[index].is_none() {
            self.leases[index] = Some(
                self.controller
                    .acquire_paced(
                        Some(ImageSlot::new(index as u32, self.leases.len() as u32).unwrap()),
                        deadline,
                        maximum,
                    )
                    .map_err(D3d12Frontend::error)?,
            );
        }
        Ok(())
    }
}
