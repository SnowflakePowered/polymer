use super::*;
impl PacingSource for D3d11Frontend {
    fn update_latency(&self) {
        let Some(mut state) = self.state.as_ref().and_then(OwnerCell::enter) else {
            return;
        };
        self.refresh_latency(&mut state);
    }
}
impl D3d11Frontend {
    pub fn register(self: &Arc<Self>) -> windows::core::Result<()> {
        if self.pacing.is_some() {
            let owner: Arc<dyn PacingSource> = self.clone();
            DxgiMaintenance::register(Arc::downgrade(&owner))?;
        }
        Ok(())
    }
}

impl D3d11Frontend {
    pub(crate) fn bind(
        &mut self,
        registration: SourceRegistration,
        window: polymer_platform_windows::FixedWindowBinding,
        waitable: bool,
    ) -> windows::core::Result<()> {
        self.registration = Some(registration);
        self.fixed_window = Some(window);
        if waitable {
            self.pacing = Some(FrameLatency::new()?);
        }
        Ok(())
    }
    pub(super) fn refresh_latency(&self, state: &mut State) {
        let Some(pacing) = &self.pacing else {
            return;
        };
        if state.failed || state.controller.poll().is_err() {
            state.failed = true;
            pacing.signal(true);
            return;
        }
        match state
            .controller
            .wait_for_pacing(pacing.maximum(), AcquireDeadline::Nonblocking)
            .map_err(Self::error)
        {
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
        let mut state = self.enter(AcquireDeadline::after(Duration::from_secs(2)))?;
        pacing.set_maximum(maximum);
        self.refresh_latency(&mut state);
        Ok(())
    }
}
