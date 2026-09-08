use super::*;

impl VirtualDxgiDriver for D3d12Frontend {
    fn destroyed(&self) {
        if let Some(registration) = &self.registration {
            registration.release();
        }
    }

    fn buffer(&self, index: u32) -> windows::core::Result<IUnknown> {
        self.resources
            .get(index as usize)
            .ok_or_else(|| Error::from_hresult(DXGI_ERROR_INVALID_CALL))?
            .cast()
    }
    fn present(&self, _: u32, flags: DXGI_PRESENT) -> HRESULT {
        let deadline =
            if flags.contains(DXGI_PRESENT_DO_NOT_WAIT) || flags.contains(DXGI_PRESENT_TEST) {
                AcquireDeadline::Nonblocking
            } else {
                AcquireDeadline::after(Duration::from_secs(2))
            };
        let mut state = match self
            .state
            .as_ref()
            .expect("live frontend")
            .enter_until(deadline)
        {
            Ok(state) => state,
            Err(error) => return Self::owner_error(error).code(),
        };
        if flags.contains(DXGI_PRESENT_TEST) {
            return if state.failed {
                DXGI_ERROR_DEVICE_REMOVED
            } else {
                state
                    .controller
                    .poll()
                    .map_or_else(|error| Self::error(error).code(), |()| S_OK)
            };
        }
        let index = self.current.load(Ordering::Acquire) as usize;
        if let Err(error) = state.acquire(
            index,
            deadline,
            self.pacing.as_ref().map(|pacing| pacing.maximum()),
        ) {
            return error.code();
        }
        let next = (index + 1) % self.resources.len();
        if flags.contains(DXGI_PRESENT_DO_NOT_WAIT) {
            if let Err(error) = state.acquire(next, AcquireDeadline::Nonblocking, None) {
                return error.code();
            }
        }
        let lease = state.leases[index].take().unwrap();
        if let Some(pacing) = &self.pacing {
            pacing.signal(false);
        }
        match state.controller.present(
            lease,
            (),
            polymer_platform_windows::performance_counter().unwrap_or(0),
        ) {
            Ok(_) => {
                if let Err(error) = state.acquire(next, deadline, None) {
                    state.failed = true;
                    if let Some(pacing) = &self.pacing {
                        pacing.signal(true);
                    }
                    return if error.code() == DXGI_ERROR_WAS_STILL_DRAWING {
                        DXGI_ERROR_DEVICE_REMOVED
                    } else {
                        error.code()
                    };
                }
                self.current.store(next as u32, Ordering::Release);
                self.refresh_latency(&mut state);
                S_OK
            }
            Err(error) => {
                state.failed = true;
                if let Some(pacing) = &self.pacing {
                    pacing.signal(true);
                }
                Self::error(error).code()
            }
        }
    }
    fn current_index(&self) -> u32 {
        self.current.load(Ordering::Acquire)
    }
    fn set_maximum_latency(&self, maximum: u32) -> windows::core::Result<()> {
        self.set_latency(maximum)
    }
    fn maximum_latency(&self) -> u32 {
        self.pacing.as_ref().map_or(1, |pacing| pacing.maximum())
    }
    fn latency_handle(&self) -> HANDLE {
        self.pacing
            .as_ref()
            .map_or(HANDLE::default(), |pacing| pacing.duplicate())
    }
}
