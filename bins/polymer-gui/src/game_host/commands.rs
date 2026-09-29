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

    pub fn save_state(
        &self,
        timeout: std::time::Duration,
    ) -> Result<(u32, PendingReply<StateAction>), String> {
        let active = self
            .active
            .as_ref()
            .ok_or_else(|| "emulator is no longer active".to_owned())?;
        if active.closing {
            return Err("emulator is closing".to_owned());
        }
        let states = active
            .save_states
            .as_ref()
            .ok_or_else(|| "emulator does not support save states".to_owned())?;
        let session = active
            .controller_session
            .as_ref()
            .ok_or_else(|| "emulator plugin session is unavailable".to_owned())?;
        let slot = states.next_slot();
        Ok((
            slot,
            Box::pin(session.send(StateAction {
                slot,
                save: true,
                timeout,
            })),
        ))
    }

    pub fn resume_state(
        &self,
        slot: u32,
        timeout: std::time::Duration,
    ) -> Result<PendingReply<StateAction>, String> {
        let active = self
            .active
            .as_ref()
            .ok_or_else(|| "emulator is no longer active".to_owned())?;
        if active.closing {
            return Err("emulator is closing".to_owned());
        }
        let states = active
            .save_states
            .as_ref()
            .ok_or_else(|| "emulator does not support save states".to_owned())?;
        if !states.contains(slot) {
            return Err(format!("save-state slot {slot} does not exist"));
        }
        let session = active
            .controller_session
            .as_ref()
            .ok_or_else(|| "emulator plugin session is unavailable".to_owned())?;
        Ok(Box::pin(session.send(StateAction {
            slot,
            save: false,
            timeout,
        })))
    }

    pub fn commit_save_state(
        &mut self,
        slot: u32,
    ) -> Result<polymer_runtime::SaveStateMetadata, String> {
        let screenshot = self.capture_primary_screenshot();
        let states = self
            .active
            .as_mut()
            .and_then(|active| active.save_states.as_mut())
            .ok_or_else(|| "emulator does not support save states".to_owned())?;
        Ok(states.commit(slot, std::time::SystemTime::now(), screenshot))
    }

    pub fn capture_primary_screenshot(&mut self) -> Option<polymer_runtime::RgbaScreenshot> {
        let connection = self
            .surfaces
            .iter()
            .find_map(|(&connection, surface)| (surface.index == 0).then_some(connection))?;
        let captured = self.compositor.borrow_mut().snapshot(connection);
        match captured {
            Ok(screenshot) => screenshot,
            Err(error) => {
                tracing::warn!("polymer: primary-surface screenshot failed: {error}");
                None
            }
        }
    }

    pub fn save_state_metadata(&self, slot: u32) -> Option<polymer_runtime::SaveStateMetadata> {
        self.active
            .as_ref()?
            .save_states
            .as_ref()?
            .metadata_for_slot(slot)
    }

    pub fn save_state_screenshot(&self, record_id: &str) -> Result<Option<Vec<u8>>, String> {
        let states = self
            .active
            .as_ref()
            .and_then(|active| active.save_states.as_ref())
            .ok_or_else(|| "emulator does not support save states".to_owned())?;
        Ok(states.screenshot(record_id).map(<[u8]>::to_vec))
    }

    pub fn begin_quit(
        &mut self,
        token: crate::shell_runtime::InstanceToken,
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
        let screenshot = self
            .capture_primary_screenshot()
            .and_then(|screenshot| match screenshot.encode_png() {
                Ok(png) => Some(png),
                Err(error) => {
                    tracing::warn!("polymer: Quick Resume screenshot encoding failed: {error}");
                    None
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
