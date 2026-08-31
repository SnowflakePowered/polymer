use super::{DeviceEntry, HubError, HubInner};
use crate::{
    BackendId, Calibration, CalibrationError, CalibrationKind, DeviceDescriptor, DeviceId,
    DeviceState, Feedback, FeedbackClass, InputEvent, PlayerIndex, PointerSample,
    TriggerCalibration,
};

impl HubInner {
    pub(crate) fn connected(
        self: &std::sync::Arc<Self>,
        backend: BackendId,
        descriptor: DeviceDescriptor,
    ) {
        if descriptor.id.backend() != backend {
            tracing::warn!(%backend, device = %descriptor.id, "input backend published a foreign device");
            return;
        }
        let (listeners, index, calibration, home_led) = {
            let mut state = self.state.lock();
            let id = descriptor.id.clone();
            let entry = state
                .devices
                .entry(id.clone())
                .or_insert_with(|| DeviceEntry {
                    descriptor: descriptor.clone(),
                    state: DeviceState::default(),
                    pointer: PointerSample::None,
                    pointer_revision: 0,
                    connected: true,
                });
            entry.descriptor = descriptor.clone();
            entry.connected = true;
            let index = state.index_of(&id);
            let calibration = state.calibrations.get(&id).cloned();
            let home_led = state.home_led.get(&id).copied();
            let listeners = state.listeners.values().cloned().collect::<Vec<_>>();
            (listeners, index, calibration, home_led)
        };
        if let Some(calibration) = calibration {
            let _ = self.apply_calibration(&descriptor.id, &calibration);
        }
        if let Some(brightness) = home_led {
            let _ = self.apply_device_feedback(&descriptor.id, &Feedback::HomeLed { brightness });
        }
        for listener in listeners {
            listener.on_input(InputEvent::Connected {
                index,
                device: &descriptor,
            });
        }
        let auto_compose = self.state.lock().policy.auto_compose;
        if auto_compose
            && matches!(
                descriptor.role,
                crate::DeviceRole::LeftHalf | crate::DeviceRole::RightHalf
            )
        {
            let revision = self
                .composition_revision
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                .wrapping_add(1);
            let hub = std::sync::Arc::downgrade(self);
            let _ = std::thread::Builder::new()
                .name("snowglass-input-compose".to_owned())
                .spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    let Some(hub) = hub.upgrade() else {
                        return;
                    };
                    if hub
                        .composition_revision
                        .load(std::sync::atomic::Ordering::Relaxed)
                        == revision
                    {
                        hub.try_auto_compose();
                    }
                });
        } else {
            self.try_auto_assign(&descriptor.id);
        }
    }

    pub(crate) fn disconnected(&self, backend: BackendId, device: &DeviceId) {
        if device.backend() != backend {
            return;
        }
        let (listeners, index, existed, retain) = {
            let mut state = self.state.lock();
            let index = state.index_of(device);
            let existed = if let Some(entry) = state.devices.get_mut(device) {
                entry.connected = false;
                true
            } else {
                false
            };
            let retain = state.policy.retain_disconnected;
            if !retain {
                state.assignments.retain(|_, assigned| assigned != device);
                state.devices.remove(device);
            }
            (
                state.listeners.values().cloned().collect::<Vec<_>>(),
                index,
                existed,
                retain,
            )
        };
        if !existed {
            return;
        }
        for listener in listeners {
            listener.on_input(InputEvent::Disconnected { index, device });
        }
        if retain {
            self.update_composites(device);
        }
    }

    pub(crate) fn publish(&self, backend: BackendId, device: &DeviceId, value: &DeviceState) {
        if device.backend() != backend {
            return;
        }
        let event = {
            let mut state = self.state.lock();
            let Some(entry) = state.devices.get_mut(device) else {
                return;
            };
            if !entry.connected {
                return;
            }
            entry.state = *value;
            if let Some(capture) = state.captures.get_mut(device) {
                capture.accept(value);
            }
            let Some(index) = state.index_of(device) else {
                drop(state);
                self.update_composites(device);
                return;
            };
            let pointer = state.pointer_overrides.get(&index).copied();
            let sequence = state.sequences.entry(index).or_default();
            *sequence = sequence.wrapping_add(1);
            let sequence = *sequence;
            let mut published = *value;
            if let Some(pointer) = pointer {
                published.pointer = pointer;
            }
            (
                state.listeners.values().cloned().collect::<Vec<_>>(),
                index,
                sequence,
                published,
            )
        };
        for listener in event.0 {
            listener.on_input(InputEvent::State {
                index: event.1,
                sequence: event.2,
                device,
                state: &event.3,
            });
        }
        self.update_composites(device);
    }

    pub(crate) fn publish_pointer(
        &self,
        backend: BackendId,
        device: &DeviceId,
        sample: &PointerSample,
    ) {
        if device.backend() != backend {
            return;
        }
        let revision = self
            .next_pointer_revision
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Some(entry) = self.state.lock().devices.get_mut(device) {
            entry.pointer = *sample;
            entry.pointer_revision = revision;
        }
        self.update_composites(device);
    }

    pub(crate) fn publish_raw_hid(&self, backend: BackendId, device: &DeviceId, report: &[u8]) {
        if device.backend() != backend {
            return;
        }
        let (listeners, index, descriptor) = {
            let state = self.state.lock();
            let Some(index) = state.index_of(device) else {
                return;
            };
            let Some(entry) = state.devices.get(device) else {
                return;
            };
            (
                state.listeners.values().cloned().collect::<Vec<_>>(),
                index,
                entry.descriptor.clone(),
            )
        };
        for listener in listeners {
            if listener.wants_raw_hid(&descriptor) {
                listener.on_input(InputEvent::RawHid {
                    index,
                    device,
                    report,
                });
            }
        }
    }

    pub(crate) fn feedback(
        &self,
        listener: crate::ListenerId,
        index: PlayerIndex,
        feedback: Feedback,
    ) -> Result<(), HubError> {
        {
            let state = self.state.lock();
            if !state.listeners.contains_key(&listener) {
                return Err(HubError::ListenerNotFound(listener));
            }
            match feedback.class() {
                FeedbackClass::Actuation
                    if state.feedback_priority.is_some()
                        && state.feedback_priority != Some(listener) =>
                {
                    return Err(HubError::FeedbackNotOwned);
                }
                FeedbackClass::Managed => return Err(HubError::HubOwned),
                _ => {}
            }
        }
        if let Feedback::Calibrate(calibration) = &feedback {
            let devices = self.devices_for_index(index)?;
            for device in devices {
                self.calibrate(&device, calibration)?;
            }
            return Ok(());
        }
        let devices = self.devices_for_index(index)?;
        for device in devices {
            if let Feedback::HomeLed { brightness } = feedback {
                self.state
                    .lock()
                    .home_led
                    .insert(device.clone(), brightness);
            }
            self.apply_device_feedback(&device, &feedback)?;
        }
        Ok(())
    }

    pub(crate) fn for_each_device(
        &self,
        index: PlayerIndex,
        mut operation: impl FnMut(
            &dyn crate::InputBackend,
            &DeviceId,
        ) -> Result<(), crate::BackendError>,
    ) -> Result<(), HubError> {
        let targets = {
            let state = self.state.lock();
            self.devices_for_index_locked(&state, index)?
                .into_iter()
                .map(|device| {
                    let backend = state
                        .backends
                        .get(&device.backend())
                        .ok_or(HubError::BackendUnavailable(device.backend()))?
                        .clone();
                    Ok((backend, device))
                })
                .collect::<Result<Vec<_>, HubError>>()?
        };
        for (backend, device) in targets {
            operation(backend.as_ref(), &device)?;
        }
        Ok(())
    }

    fn devices_for_index(&self, index: PlayerIndex) -> Result<Vec<DeviceId>, HubError> {
        let state = self.state.lock();
        self.devices_for_index_locked(&state, index)
    }

    fn devices_for_index_locked(
        &self,
        state: &super::HubState,
        index: PlayerIndex,
    ) -> Result<Vec<DeviceId>, HubError> {
        let device = state
            .assignments
            .get(&index)
            .ok_or(HubError::InvalidPlayerIndex(index))?;
        Ok(state.composites.get(device).map_or_else(
            || vec![device.clone()],
            |composite| composite.members.clone(),
        ))
    }

    pub(crate) fn calibrate(
        &self,
        device: &DeviceId,
        update: &Calibration,
    ) -> Result<(), HubError> {
        let calibration = {
            let mut state = self.state.lock();
            if !state.devices.contains_key(device) {
                return Err(HubError::DeviceNotFound(device.clone()));
            }
            let calibration = state.calibrations.entry(device.clone()).or_default();
            calibration.merge(update);
            calibration.clone()
        };
        self.apply_calibration(device, &calibration)
    }

    pub(crate) fn set_calibration(
        &self,
        device: &DeviceId,
        calibration: Calibration,
    ) -> Result<(), HubError> {
        {
            let mut state = self.state.lock();
            if !state.devices.contains_key(device) {
                return Err(HubError::DeviceNotFound(device.clone()));
            }
            state
                .calibrations
                .insert(device.clone(), calibration.clone());
        }
        self.apply_calibration(device, &calibration)
    }

    pub(crate) fn apply_calibration(
        &self,
        device: &DeviceId,
        calibration: &Calibration,
    ) -> Result<(), HubError> {
        let backend = self
            .state
            .lock()
            .backends
            .get(&device.backend())
            .ok_or(HubError::BackendUnavailable(device.backend()))?
            .clone();
        backend.set_calibration(device, calibration)?;
        Ok(())
    }

    fn apply_device_feedback(
        &self,
        device: &DeviceId,
        feedback: &Feedback,
    ) -> Result<(), HubError> {
        let backend = self
            .state
            .lock()
            .backends
            .get(&device.backend())
            .ok_or(HubError::BackendUnavailable(device.backend()))?
            .clone();
        match feedback {
            Feedback::Recenter => backend.recenter(device)?,
            _ => backend.apply_feedback(device, feedback)?,
        }
        Ok(())
    }

    pub(crate) fn finish_calibration(&self, device: &DeviceId) -> Result<(), CalibrationError> {
        let (capture, mut calibration) = {
            let mut state = self.state.lock();
            let capture = state
                .captures
                .remove(device)
                .ok_or(CalibrationError::Cancelled)?;
            let calibration = state.calibrations.get(device).cloned().unwrap_or_default();
            (capture, calibration)
        };
        if capture.samples == 0 {
            self.cancel_calibration(device);
            return Err(CalibrationError::InsufficientSamples);
        }
        match capture.kind {
            CalibrationKind::StickCenter => {
                for stick in 0..2 {
                    let current = calibration.sticks[stick].get_or_insert_default();
                    for axis in 0..2 {
                        let observed = capture.stick_sum[stick][axis] / i64::from(capture.samples);
                        let center = i64::from(current.center[axis]);
                        let range = if observed < 0 {
                            center - i64::from(current.min[axis])
                        } else {
                            i64::from(current.max[axis]) - center
                        }
                        .max(1);
                        let correction = observed * range / i64::from(i16::MAX);
                        current.center[axis] = (center + correction)
                            .clamp(i64::from(i16::MIN), i64::from(i16::MAX))
                            as i16;
                    }
                }
            }
            CalibrationKind::StickRange => {
                for stick in 0..2 {
                    let current = calibration.sticks[stick].get_or_insert_default();
                    for axis in 0..2 {
                        let center = i64::from(current.center[axis]);
                        let negative_range = (center - i64::from(current.min[axis])).max(1);
                        let positive_range = (i64::from(current.max[axis]) - center).max(1);
                        current.min[axis] = (center
                            + i64::from(capture.stick_min[stick][axis]) * negative_range
                                / i64::from(i16::MAX))
                        .clamp(i64::from(i16::MIN), i64::from(i16::MAX))
                            as i16;
                        current.max[axis] = (center
                            + i64::from(capture.stick_max[stick][axis]) * positive_range
                                / i64::from(i16::MAX))
                        .clamp(i64::from(i16::MIN), i64::from(i16::MAX))
                            as i16;
                    }
                }
            }
            CalibrationKind::TriggerRange => {
                for trigger in 0..2 {
                    let current =
                        calibration.triggers[trigger].get_or_insert(TriggerCalibration::default());
                    let min = u64::from(current.min);
                    let range = u64::from(current.max.saturating_sub(current.min).max(1));
                    current.min = (min
                        + u64::from(capture.trigger_min[trigger]) * range / u64::from(u16::MAX))
                    .min(u64::from(u16::MAX)) as u16;
                    current.max = (min
                        + u64::from(capture.trigger_max[trigger]) * range / u64::from(u16::MAX))
                    .min(u64::from(u16::MAX)) as u16;
                }
            }
            CalibrationKind::MotionBias => {
                let current = calibration.motion.get_or_insert_default();
                for axis in 0..3 {
                    let observed = capture.gyro_sum[axis] / f64::from(capture.samples);
                    let scale = f64::from(current.gyroscope_scale[axis]);
                    if scale.abs() > f64::EPSILON {
                        current.gyroscope_bias[axis] += (observed / scale) as f32;
                    }
                }
            }
        }
        self.set_calibration(device, calibration)
            .map_err(|_| CalibrationError::DeviceUnavailable)
    }

    pub(crate) fn cancel_calibration(&self, device: &DeviceId) {
        let calibration = {
            let mut state = self.state.lock();
            state.captures.remove(device);
            state.calibrations.get(device).cloned().unwrap_or_default()
        };
        let _ = self.apply_calibration(device, &calibration);
    }
}
