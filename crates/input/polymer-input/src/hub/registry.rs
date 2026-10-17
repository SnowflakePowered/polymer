use super::{CalibrationSession, DeviceEntry, HubError, HubInner};
use crate::{
    BackendId, DeviceDescriptor, DeviceId, DeviceState, Feedback, FeedbackClass, InputEvent,
    PlayerIndex, PointerSample,
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
        let (listeners, index, calibration, home_led, inherited) = {
            let _topology = self.topology.write();
            let id = descriptor.id.clone();
            let mut entry = self
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
            drop(entry);
            let index = self.index_of(&id);
            let inherited = descriptor
                .serial
                .filter(|_| {
                    index.is_none()
                        && descriptor.role == crate::DeviceRole::Whole
                        && self.policy.read().auto_assign
                })
                .and_then(|serial| {
                    self.devices
                        .iter()
                        .filter(|entry| {
                            entry.descriptor.id.backend() != backend
                                && entry.descriptor.serial == Some(serial)
                        })
                        .find_map(|entry| self.index_of(&entry.descriptor.id))
                });
            let calibration = self
                .calibration_sessions
                .get(&id)
                .map(|session| session.feedback());
            let home_led = self.home_led.get(&id).map(|value| *value);
            let listeners = self.listeners();
            (listeners, index, calibration, home_led, inherited)
        };
        if let Some(calibration) = calibration {
            let _ = self.apply_feedback(&descriptor.id, &calibration);
        }
        if let Some(brightness) = home_led {
            let _ = self.apply_feedback(&descriptor.id, &Feedback::HomeLed { brightness });
        }
        for listener in listeners {
            listener.on_input(InputEvent::Connected {
                index,
                device: &descriptor,
            });
        }
        if let Some(index) = inherited {
            let _ = self.assign(&descriptor.id, Some(index), false);
            return;
        }
        let auto_compose = self.policy.read().auto_compose;
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
                .name("polymer-input-compose".to_owned())
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
            let _topology = self.topology.write();
            let index = self.index_of(device);
            let existed = if let Some(mut entry) = self.devices.get_mut(device) {
                entry.connected = false;
                true
            } else {
                false
            };
            let retain = self.policy.read().retain_disconnected;
            if !retain {
                self.assignments.retain(|_, assigned| assigned != device);
                self.devices.remove(device);
            }
            (self.listeners(), index, existed, retain)
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
            let topology = self.topology.read();
            let Some(mut entry) = self.devices.get_mut(device) else {
                return;
            };
            if !entry.connected {
                return;
            }
            entry.state = *value;
            drop(entry);
            if let Some(mut session) = self.calibration_sessions.get_mut(device) {
                session.accept(value);
            }
            let Some(index) = self.index_of(device) else {
                drop(topology);
                self.update_composites(device);
                return;
            };
            let pointer = self.pointer_overrides.get(&index).map(|value| *value);
            let mut sequence = self.sequences.entry(index).or_default();
            *sequence = sequence.wrapping_add(1);
            let sequence = *sequence;
            let mut published = *value;
            if let Some(pointer) = pointer {
                published.pointer = pointer;
            }
            (self.listeners(), index, sequence, published)
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
        if let Some(mut entry) = self.devices.get_mut(device) {
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
            let _topology = self.topology.read();
            let Some(index) = self.index_of(device) else {
                return;
            };
            let Some(entry) = self.devices.get(device) else {
                return;
            };
            (self.listeners(), index, entry.descriptor.clone())
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

    pub(crate) fn dispatch_feedback(
        &self,
        listener: crate::ListenerId,
        index: PlayerIndex,
        feedback: Feedback,
    ) -> Result<(), HubError> {
        {
            if !self.listeners.contains_key(&listener) {
                return Err(HubError::ListenerNotFound(listener));
            }
            let priority = *self.feedback_priority.read();
            match feedback.class() {
                FeedbackClass::Actuation if priority.is_some() && priority != Some(listener) => {
                    return Err(HubError::FeedbackNotOwned);
                }
                FeedbackClass::Managed => return Err(HubError::HubOwned),
                _ => {}
            }
        }
        if let Feedback::Calibrate(calibration) = &feedback {
            let devices = self.devices_for_index(index)?;
            for device in devices {
                self.update_calibration(&device, |session| session.merge(calibration))?;
            }
            return Ok(());
        }
        let devices = self.devices_for_index(index)?;
        for device in devices {
            if let Feedback::HomeLed { brightness } = feedback {
                self.home_led.insert(device.clone(), brightness);
            }
            self.apply_feedback(&device, &feedback)?;
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
        let targets = self
            .devices_for_index(index)?
            .into_iter()
            .map(|device| {
                let backend = self
                    .backends
                    .get(&device.backend())
                    .ok_or(HubError::BackendUnavailable(device.backend()))?
                    .clone();
                Ok((backend, device))
            })
            .collect::<Result<Vec<_>, HubError>>()?;
        for (backend, device) in targets {
            operation(backend.as_ref(), &device)?;
        }
        Ok(())
    }

    fn devices_for_index(&self, index: PlayerIndex) -> Result<Vec<DeviceId>, HubError> {
        let _topology = self.topology.read();
        let device = self
            .assignments
            .get(&index)
            .ok_or(HubError::InvalidPlayerIndex(index))?
            .value()
            .clone();
        Ok(self.composites.get(&device).map_or_else(
            || vec![device.clone()],
            |composite| composite.members.clone(),
        ))
    }

    pub(crate) fn update_calibration(
        &self,
        device: &DeviceId,
        update: impl FnOnce(&mut CalibrationSession) -> Feedback,
    ) -> Result<(), HubError> {
        let feedback = {
            let _topology = self.topology.read();
            if !self.devices.contains_key(device) {
                return Err(HubError::DeviceNotFound(device.clone()));
            }
            update(&mut self.calibration_sessions.entry(device.clone()).or_default())
        };
        self.apply_feedback(device, &feedback)
    }

    pub(crate) fn apply_feedback(
        &self,
        device: &DeviceId,
        feedback: &Feedback,
    ) -> Result<(), HubError> {
        let backend = self
            .backends
            .get(&device.backend())
            .ok_or(HubError::BackendUnavailable(device.backend()))?
            .clone();
        backend.apply_feedback(device, feedback)?;
        Ok(())
    }
}
