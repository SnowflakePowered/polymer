use super::*;
impl PacingSource for D3d12Frontend {
    fn update_latency(&self) {
        let Some(mut state) = self.state.as_ref().and_then(OwnerCell::enter) else {
            return;
        };
        self.refresh_latency(&mut state);
    }
}
impl D3d12Frontend {
    pub fn register(self: &Arc<Self>) -> windows::core::Result<()> {
        if self.pacing.is_some() {
            let owner: Arc<dyn PacingSource> = self.clone();
            DxgiMaintenance::register(Arc::downgrade(&owner))?;
        }
        Ok(())
    }
}

impl D3d12Frontend {
    pub(crate) fn enable_waitable(mut self, enabled: bool) -> windows::core::Result<Self> {
        if enabled {
            self.pacing = Some(FrameLatency::new()?);
        }
        Ok(self)
    }
    pub(super) fn refresh_latency(&self, state: &mut State) {
        let Some(pacing) = &self.pacing else {
            return;
        };
        if state.controller.poll().is_err() {
            state.failed = true;
            pacing.signal(true);
            return;
        }
        let index = self.current.load(Ordering::Acquire) as usize;
        match state.acquire(index, AcquireDeadline::Nonblocking, Some(pacing.maximum())) {
            Ok(()) => pacing.signal(true),
            Err(error) if error.code() == DXGI_ERROR_WAS_STILL_DRAWING => pacing.signal(false),
            Err(_) => {
                state.failed = true;
                // Wake a waiting client so Present can report device loss.
                pacing.signal(true);
            }
        }
    }
    pub(super) fn set_latency(&self, maximum: u32) -> windows::core::Result<()> {
        let pacing = self
            .pacing
            .as_ref()
            .ok_or_else(|| Error::from_hresult(DXGI_ERROR_INVALID_CALL))?;
        if !(1..=16).contains(&maximum) {
            return Err(Error::from_hresult(DXGI_ERROR_INVALID_CALL));
        }
        let mut state = self
            .state
            .as_ref()
            .expect("live frontend")
            .enter_until(AcquireDeadline::after(Duration::from_secs(2)))
            .map_err(Self::owner_error)?;
        pacing.set_maximum(maximum);
        self.refresh_latency(&mut state);
        Ok(())
    }
}
