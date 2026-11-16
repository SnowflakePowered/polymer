use super::*;

impl GameHost {
    pub fn controller_change(
        &self,
        change: polymer_controllers::ControllerTopologyChange,
    ) -> Result<Option<PendingReply<ControllerChanged>>, String> {
        let Some(active) = &self.active else {
            return Ok(None);
        };
        let Some(session) = &active.controller_session else {
            return Ok(None);
        };
        tracing::info!(
            token = active.token.0,
            previous = %change.previous.assignment_diagnostics(),
            requested = %change.requested.assignment_diagnostics(),
            "polymer: host queued controller topology for plugin"
        );
        Ok(Some(Box::pin(session.send(ControllerChanged(change)))))
    }

    pub fn pause_resume(
        &self,
        state: polymer_pluginhost::controller::PauseState,
        timeout: std::time::Duration,
    ) -> Result<Option<PendingReply<PauseResume>>, String> {
        let Some(active) = &self.active else {
            return Ok(None);
        };
        let Some(session) = &active.controller_session else {
            return Ok(None);
        };
        Ok(Some(Box::pin(session.send(PauseResume { state, timeout }))))
    }

    pub fn change_disc(&self, index: u32) -> Result<PendingReply<DiscChange>, String> {
        let active = self.active.as_ref().ok_or("emulator is no longer active")?;
        if active.closing {
            return Err("emulator is closing".to_owned());
        }
        if !active.options.can_change_disc || active.options.launch_set.len() < 2 {
            return Err("emulator does not support disc changes".to_owned());
        }
        if index as usize >= active.options.launch_set.len() {
            return Err("disc index is outside the launch set".to_owned());
        }
        let session = active
            .controller_session
            .as_ref()
            .ok_or("emulator plugin session is unavailable")?;
        Ok(Box::pin(session.send(DiscChange {
            index,
            timeout: Duration::from_secs(30),
        })))
    }

    pub fn commit_disc_change(&mut self, index: u32) -> Result<(), String> {
        let active = self.active.as_mut().ok_or("emulator is no longer active")?;
        active.current_disc_index = index;
        Ok(())
    }

    pub fn state_action(
        &self,
        slot: u32,
        save: bool,
        timeout: std::time::Duration,
    ) -> Result<PendingReply<StateAction>, String> {
        let active = self
            .active
            .as_ref()
            .ok_or_else(|| "emulator is no longer active".to_owned())?;
        if active.closing {
            return Err("emulator is closing".to_owned());
        }
        let max = active
            .options
            .save_states
            .as_ref()
            .ok_or_else(|| "emulator does not support save states".to_owned())?
            .max;
        if slot >= max.get() {
            return Err(format!("save-state slot {slot} is outside 0..{max}"));
        }
        let session = active
            .controller_session
            .as_ref()
            .ok_or_else(|| "emulator plugin session is unavailable".to_owned())?;
        Ok(Box::pin(session.send(StateAction {
            slot,
            save,
            timeout,
        })))
    }

    pub fn capture_surface(
        &mut self,
        index: u32,
    ) -> Result<Option<polymer_runtime::RgbaScreenshot>, String> {
        let Some(connection) = self
            .surfaces
            .iter()
            .find_map(|(&connection, surface)| (surface.index == index).then_some(connection))
        else {
            return Ok(None);
        };
        self.compositor.borrow_mut().snapshot(connection)
    }

    pub fn capture_primary_screenshot(&mut self) -> Option<polymer_runtime::RgbaScreenshot> {
        self.capture_surface(0).unwrap_or_else(|error| {
            tracing::warn!("polymer: primary-surface screenshot failed: {error}");
            None
        })
    }

    #[allow(clippy::type_complexity)]
    pub fn begin_quit(
        &mut self,
        token: crate::shell::runtime::InstanceToken,
        timeout: std::time::Duration,
    ) -> Result<
        Option<(
            crate::launch::QuickResumeCache,
            Option<Vec<u8>>,
            PendingReply<Quit>,
        )>,
        String,
    > {
        let cache = {
            let Some(active) = self.active.as_mut().filter(|active| active.token == token) else {
                return Ok(None);
            };
            if active.closing {
                return Err("emulator is already closing".to_owned());
            }
            active.closing = true;
            active.options.quick_resume_cache.clone().map(|mut cache| {
                cache.disc_index = active.current_disc_index;
                cache
            })
        };
        let Some(cache) = cache else {
            return Ok(None);
        };
        let screenshot =
            self.capture_primary_screenshot().and_then(|screenshot| {
                match screenshot.encode(image::ImageFormat::Png) {
                    Ok(png) => Some(png),
                    Err(error) => {
                        tracing::warn!("polymer: Quick Resume screenshot encoding failed: {error}");
                        None
                    }
                }
            });
        let Some(session) = self
            .active
            .as_ref()
            .and_then(|active| active.controller_session.as_ref())
        else {
            return Err("Quick Resume plugin session is unavailable".to_owned());
        };
        Ok(Some((
            cache,
            screenshot,
            Box::pin(session.send(Quit { timeout })),
        )))
    }
}
