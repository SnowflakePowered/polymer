mod pacing;
mod retirement;
use super::*;
use polymer_hook_dxgi::swapchain::VirtualDxgiDriver;
use std::{sync::Arc, time::Duration};
use windows::core::{HRESULT, IUnknown};

struct State {
    controller: SwapchainController<D3d11VirtualBackend>,
    lease: Option<AppLease>,
    failed: bool,
}

/// Stable native buffer zero. The driver rotates its backing allocation;
/// cached views and native command lists need no interception.
pub struct D3d11Frontend {
    state: Option<OwnerCell<State>>,
    texture: ID3D11Texture2D,
    registration: Option<SourceRegistration>,
    fixed_window: Option<polymer_platform_windows::FixedWindowBinding>,
    pacing: Option<FrameLatency>,
}

impl D3d11Frontend {
    pub fn new(
        mut controller: SwapchainController<D3d11VirtualBackend>,
        context: ID3D11DeviceContext,
    ) -> Result<Arc<Self>, Error> {
        if unsafe { context.GetType() } != D3D11_DEVICE_CONTEXT_IMMEDIATE {
            return Err(Error::new(
                DXGI_ERROR_INVALID_CALL,
                "virtual presentation requires the device's immediate context",
            ));
        }
        let count = controller.profile().image_count;
        let texture = controller
            .api_access(ImageSlot::new(0, count).unwrap())
            .map_err(Self::error)?;
        if unsafe { texture.GetDevice()? }.cast::<IUnknown>()?
            != unsafe { context.GetDevice()? }.cast::<IUnknown>()?
        {
            return Err(Error::new(
                DXGI_ERROR_INVALID_CALL,
                "native buffer context belongs to another device",
            ));
        }
        let lease = controller
            .acquire(None, AcquireDeadline::after(Duration::from_secs(2)))
            .map_err(Self::error)?;
        let frontend = Arc::new(Self {
            state: Some(OwnerCell::new(State {
                controller,
                lease: Some(lease),
                failed: false,
            })),
            texture,
            registration: None,
            fixed_window: None,
            pacing: None,
        });
        Ok(frontend)
    }
}
impl D3d11Frontend {
    pub fn retire(mut self) -> BackendRetirement<D3d11VirtualBackend> {
        self.state
            .take()
            .expect("live frontend")
            .into_inner()
            .expect("idle frontend")
            .controller
            .retire(RetireReason::ApplicationDestroyed)
    }
    fn error(error: ControllerError<Error>) -> Error {
        match error {
            ControllerError::Backend(error) => error,
            ControllerError::WouldBlock | ControllerError::Timeout => {
                Error::from_hresult(DXGI_ERROR_WAS_STILL_DRAWING)
            }
            error => Error::new(
                DXGI_ERROR_DEVICE_REMOVED,
                format!("virtual D3D11: {error:?}"),
            ),
        }
    }
    fn enter(&self, deadline: AcquireDeadline) -> Result<OwnerCall<'_, State>, Error> {
        self.state
            .as_ref()
            .expect("live frontend")
            .enter_until(deadline)
            .map_err(|error| {
                Error::new(
                    match error {
                        OwnerUnavailable::Busy => DXGI_ERROR_WAS_STILL_DRAWING,
                        OwnerUnavailable::Reentrant => DXGI_ERROR_INVALID_CALL,
                        OwnerUnavailable::Abandoned => DXGI_ERROR_DEVICE_REMOVED,
                    },
                    format!("D3D11 context owner: {error:?}"),
                )
            })
    }
}
impl State {
    fn acquire(&mut self, deadline: AcquireDeadline) -> Result<usize, Error> {
        if self.failed {
            return Err(Error::from_hresult(DXGI_ERROR_DEVICE_REMOVED));
        }
        if self.lease.is_none() {
            self.lease = Some(
                self.controller
                    .acquire(None, deadline)
                    .map_err(D3d11Frontend::error)?,
            );
        }
        Ok(self.lease.as_ref().unwrap().slot().index() as usize)
    }
}
impl VirtualDxgiDriver for D3d11Frontend {
    fn destroyed(&self) {
        if let Some(registration) = &self.registration {
            registration.release();
        }
    }

    fn buffer(&self, index: u32) -> Result<IUnknown, Error> {
        if index != 0 {
            return Err(Error::from_hresult(DXGI_ERROR_INVALID_CALL));
        }
        self.texture.cast()
    }
    fn present(&self, _: u32, flags: DXGI_PRESENT) -> HRESULT {
        let deadline =
            if flags.contains(DXGI_PRESENT_TEST) || flags.contains(DXGI_PRESENT_DO_NOT_WAIT) {
                AcquireDeadline::Nonblocking
            } else {
                AcquireDeadline::after(Duration::from_secs(2))
            };
        let mut state = match self.enter(deadline) {
            Ok(state) => state,
            Err(error) => return error.code(),
        };
        if state.failed {
            return DXGI_ERROR_DEVICE_REMOVED;
        }
        if flags.contains(DXGI_PRESENT_TEST) {
            return state
                .controller
                .poll()
                .map_or_else(|error| Self::error(error).code(), |()| S_OK);
        }
        if let Some(pacing) = &self.pacing {
            if let Err(error) = state.controller.wait_for_pacing(pacing.maximum(), deadline) {
                return Self::error(error).code();
            }
        }
        if let Err(error) = state.acquire(deadline) {
            return error.code();
        }
        let next = if flags.contains(DXGI_PRESENT_DO_NOT_WAIT) {
            match state.controller.acquire(None, AcquireDeadline::Nonblocking) {
                Ok(lease) => Some(lease),
                Err(error) => return Self::error(error).code(),
            }
        } else {
            None
        };
        let lease = state.lease.take().unwrap();
        if let Some(pacing) = &self.pacing {
            pacing.signal(false);
        }
        match state.controller.present(
            lease,
            (),
            polymer_platform_windows::performance_counter().unwrap_or(0),
        ) {
            Ok(_) => {
                state.lease = next;
                match state.acquire(deadline) {
                    Ok(_) => {
                        self.refresh_latency(&mut state);
                        S_OK
                    }
                    Err(error) => {
                        state.failed = true;
                        if error.code() == DXGI_ERROR_WAS_STILL_DRAWING {
                            DXGI_ERROR_DEVICE_REMOVED
                        } else {
                            error.code()
                        }
                    }
                }
            }
            Err(error) => {
                state.failed = true;
                Self::error(error).code()
            }
        }
    }
    fn current_index(&self) -> u32 {
        0
    }
    fn set_maximum_latency(&self, maximum: u32) -> Result<(), Error> {
        self.set_latency(maximum)
    }
    fn maximum_latency(&self) -> u32 {
        self.pacing.as_ref().map_or(1, FrameLatency::maximum)
    }
    fn latency_handle(&self) -> HANDLE {
        self.pacing
            .as_ref()
            .map_or(HANDLE::default(), FrameLatency::duplicate)
    }
}
