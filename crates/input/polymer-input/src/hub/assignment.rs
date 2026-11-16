use super::{Composite, CompositionMode, DeviceEntry, HubError, HubInner};
use crate::{
    BackendId, Capabilities, DeviceDescriptor, DeviceId, DeviceRole, DeviceState, Feedback,
    InputEvent, PlayerIndex, PointerSample,
};
use std::sync::Arc;

impl HubInner {
    pub(crate) fn assign(
        &self,
        device: &DeviceId,
        index: Option<PlayerIndex>,
        explicit: bool,
    ) -> Result<(), HubError> {
        let events = {
            let _topology = self.topology.write();
            if !self.devices.contains_key(device) {
                return Err(HubError::DeviceNotFound(device.clone()));
            }
            if index.is_some_and(|value| value.get() >= self.policy.read().max_players) {
                return Err(HubError::InvalidPlayerIndex(index.unwrap()));
            }
            let from = self.index_of(device);
            if from == index {
                return Ok(());
            }
            if let Some(from) = from {
                self.assignments.remove(&from);
            }
            let mut events = vec![(device.clone(), from, index)];
            if let Some(index) = index
                && let Some(displaced) = self.assignments.insert(index, device.clone())
            {
                let to = explicit.then(|| self.lowest_free()).flatten();
                if let Some(to) = to {
                    self.assignments.insert(to, displaced.clone());
                }
                events.push((displaced, Some(index), to));
            }
            events
        };
        let listeners = self.listeners();
        for (device, from, to) in &events {
            for listener in &listeners {
                listener.on_input(InputEvent::Assigned {
                    device,
                    from: *from,
                    to: *to,
                });
            }
            if let Some(to) = to {
                let _ = self.apply_managed_led(device, *to);
            }
        }
        Ok(())
    }

    pub(crate) fn compose(
        &self,
        members: &[DeviceId],
        mode: CompositionMode,
        automatic: bool,
    ) -> Result<DeviceId, HubError> {
        if members.len() < 2 {
            return Err(HubError::InvalidComposition);
        }
        let composite = DeviceId::new(
            BackendId::VIRTUAL,
            &format!(
                "pair/{}",
                members
                    .iter()
                    .map(DeviceId::as_str)
                    .collect::<Vec<_>>()
                    .join("+")
            ),
        );
        let descriptor = {
            let _topology = self.topology.write();
            if self.decomposed.contains(&composite) && automatic {
                return Err(HubError::InvalidComposition);
            }
            if self.composites.contains_key(&composite) {
                return Ok(composite);
            }
            let entries = members
                .iter()
                .map(|member| {
                    if self
                        .composites
                        .iter()
                        .any(|entry| entry.members.contains(member))
                    {
                        return Err(HubError::AlreadyComposed(member.clone()));
                    }
                    self.devices
                        .get(member)
                        .map(|entry| entry.clone())
                        .ok_or_else(|| HubError::DeviceNotFound(member.clone()))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let capabilities = entries.iter().fold(Capabilities::default(), |all, entry| {
                all | entry.descriptor.capabilities
            });
            let connected = entries.iter().all(|entry| entry.connected);
            let descriptor = DeviceDescriptor {
                id: composite.clone(),
                name: Arc::from(
                    entries
                        .iter()
                        .map(|entry| entry.descriptor.name.as_ref())
                        .collect::<Vec<_>>()
                        .join(" + "),
                ),
                profile: None,
                vendor_id: None,
                product_id: None,
                serial: None,
                role: DeviceRole::Whole,
                capabilities,
            };
            let state_value = Self::composite_state(&entries, mode);
            for member in members {
                if let Some(index) = self.index_of(member) {
                    self.assignments.remove(&index);
                }
                self.individual_devices.remove(member);
            }
            self.devices.insert(
                composite.clone(),
                DeviceEntry {
                    descriptor: descriptor.clone(),
                    state: state_value,
                    pointer: Self::composite_pointer(&entries),
                    pointer_revision: self
                        .next_pointer_revision
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                    connected,
                },
            );
            self.composites.insert(
                composite.clone(),
                Composite {
                    members: members.to_vec(),
                    mode,
                },
            );
            descriptor
        };
        let listeners = self.listeners();
        for listener in listeners {
            listener.on_input(InputEvent::Connected {
                index: None,
                device: &descriptor,
            });
        }
        self.try_auto_assign(&composite);
        Ok(composite)
    }

    pub(crate) fn decompose(&self, composite: &DeviceId) -> Result<(), HubError> {
        let (members, index, listeners) = {
            let _topology = self.topology.write();
            let (_, value) = self
                .composites
                .remove(composite)
                .ok_or_else(|| HubError::DeviceNotFound(composite.clone()))?;
            let index = self.index_of(composite);
            if let Some(index) = index {
                self.assignments.remove(&index);
            }
            self.devices.remove(composite);
            self.decomposed.insert(composite.clone());
            for member in &value.members {
                self.individual_devices.insert(member.clone());
            }
            (value.members, index, self.listeners())
        };
        for listener in listeners {
            listener.on_input(InputEvent::Disconnected {
                index,
                device: composite,
            });
        }
        for member in members {
            self.try_auto_assign(&member);
        }
        Ok(())
    }

    pub(crate) fn try_auto_assign(&self, device: &DeviceId) {
        let index = {
            let _topology = self.topology.write();
            let policy = *self.policy.read();
            if !policy.auto_assign || self.index_of(device).is_some() {
                return;
            }
            let Some(entry) = self.devices.get(device) else {
                return;
            };
            if !entry.connected
                || (policy.auto_compose
                    && matches!(
                        entry.descriptor.role,
                        DeviceRole::LeftHalf | DeviceRole::RightHalf
                    )
                    && !self.individual_devices.contains(device))
            {
                return;
            }
            let Some(index) = self.lowest_free() else {
                return;
            };
            self.assignments.insert(index, device.clone());
            index
        };
        for listener in self.listeners() {
            listener.on_input(InputEvent::Assigned {
                device,
                from: None,
                to: Some(index),
            });
        }
        let _ = self.apply_managed_led(device, index);
    }

    pub(crate) fn try_auto_compose(&self) {
        let (members, individuals) = {
            let _topology = self.topology.write();
            if !self.policy.read().auto_compose {
                return;
            }
            let eligible = |role| {
                self.devices
                    .iter()
                    .filter(|entry| {
                        entry.connected
                            && entry.descriptor.id.backend() == BackendId::BLUETOOTH
                            && entry.descriptor.role == role
                            && self.index_of(&entry.descriptor.id).is_none()
                            && !self
                                .composites
                                .iter()
                                .any(|composite| composite.members.contains(&entry.descriptor.id))
                    })
                    .map(|entry| entry.descriptor.id.clone())
                    .collect::<Vec<_>>()
            };
            let left = eligible(DeviceRole::LeftHalf);
            let right = eligible(DeviceRole::RightHalf);
            if left.len() == 1 && right.len() == 1 {
                (Some(vec![left[0].clone(), right[0].clone()]), Vec::new())
            } else if !left.is_empty() || !right.is_empty() {
                let individuals = left.into_iter().chain(right).collect::<Vec<_>>();
                for device in &individuals {
                    self.individual_devices.insert(device.clone());
                }
                (None, individuals)
            } else {
                (None, Vec::new())
            }
        };
        if let Some(members) = members {
            let _ = self.compose(&members, CompositionMode::ByRole, true);
        }
        for device in individuals {
            self.try_auto_assign(&device);
        }
    }

    pub(crate) fn update_composites(&self, member: &DeviceId) {
        let composites = self
            .composites
            .iter()
            .filter(|entry| entry.members.contains(member))
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect::<Vec<_>>();
        for (id, composite) in composites {
            self.update_composite(&id, &composite);
        }
    }

    fn update_composite(&self, id: &DeviceId, composite: &Composite) {
        let event = {
            let _topology = self.topology.write();
            let entries = composite
                .members
                .iter()
                .filter_map(|member| self.devices.get(member).map(|entry| entry.clone()))
                .collect::<Vec<_>>();
            if entries.len() != composite.members.len() {
                return;
            }
            let connected = entries.iter().all(|entry| entry.connected);
            let value = Self::composite_state(&entries, composite.mode);
            let pointer = Self::composite_pointer(&entries);
            let pointer_revision = entries
                .iter()
                .map(|entry| entry.pointer_revision)
                .max()
                .unwrap_or_default();
            let Some(mut entry) = self.devices.get_mut(id) else {
                return;
            };
            let changed_connection = entry.connected != connected;
            entry.connected = connected;
            entry.state = value;
            entry.pointer = pointer;
            entry.pointer_revision = pointer_revision;
            drop(entry);
            let index = self.index_of(id);
            let sequence = index.map(|index| {
                let mut sequence = self.sequences.entry(index).or_default();
                *sequence = sequence.wrapping_add(1);
                *sequence
            });
            (
                self.listeners(),
                index,
                sequence,
                changed_connection,
                connected,
                value,
            )
        };
        for listener in event.0 {
            if event.3 {
                if event.4 {
                    let descriptor = self.devices.get(id).unwrap().descriptor.clone();
                    listener.on_input(InputEvent::Connected {
                        index: event.1,
                        device: &descriptor,
                    });
                } else {
                    listener.on_input(InputEvent::Disconnected {
                        index: event.1,
                        device: id,
                    });
                }
            }
            if let (Some(index), Some(sequence)) = (event.1, event.2)
                && event.4
            {
                listener.on_input(InputEvent::State {
                    index,
                    sequence,
                    device: id,
                    state: &event.5,
                });
            }
        }
    }

    fn composite_state(entries: &[DeviceEntry], mode: CompositionMode) -> DeviceState {
        let mut output = DeviceState::default();
        for entry in entries {
            for button in crate::GamepadButton::ALL {
                let pressure = output
                    .gamepad
                    .button_pressure(button)
                    .max(entry.state.gamepad.button_pressure(button));
                output.gamepad.pressure.values[button as usize] = pressure;
            }
            output.gamepad.pressure.mask |= entry.state.gamepad.pressure.mask;
            output.gamepad.buttons |= entry.state.gamepad.buttons;
            output.gamepad.left_trigger = output
                .gamepad
                .left_trigger
                .max(entry.state.gamepad.left_trigger);
            output.gamepad.right_trigger = output
                .gamepad
                .right_trigger
                .max(entry.state.gamepad.right_trigger);
            match (mode, entry.descriptor.role) {
                (CompositionMode::ByRole, DeviceRole::LeftHalf) => {
                    output.gamepad.left_stick = entry.state.gamepad.left_stick;
                }
                (CompositionMode::ByRole, DeviceRole::RightHalf) => {
                    output.gamepad.right_stick = entry.state.gamepad.right_stick;
                }
                _ => {
                    if output.gamepad.left_stick == [0; 2] {
                        output.gamepad.left_stick = entry.state.gamepad.left_stick;
                    }
                    if output.gamepad.right_stick == [0; 2] {
                        output.gamepad.right_stick = entry.state.gamepad.right_stick;
                    }
                }
            }
            if entry.state.sensor.sampled_at_ns >= output.sensor.sampled_at_ns {
                output.sensor = entry.state.sensor;
            }
            if entry.state.pointer.kind != crate::PointerKind::None {
                output.pointer = entry.state.pointer;
            }
            if entry.state.battery.level < output.battery.level || output.battery.level.is_none() {
                output.battery = entry.state.battery;
            }
        }
        output
    }

    fn composite_pointer(entries: &[DeviceEntry]) -> PointerSample {
        entries
            .iter()
            .map(|entry| entry.pointer)
            .find(|pointer| !matches!(pointer, PointerSample::None))
            .unwrap_or_default()
    }

    fn apply_managed_led(&self, device: &DeviceId, index: PlayerIndex) -> Result<(), HubError> {
        let members = self
            .composites
            .get(device)
            .map_or_else(|| vec![device.clone()], |value| value.members.clone());
        for member in members {
            let backend = self
                .backends
                .get(&member.backend())
                .map(|value| value.clone());
            if let Some(backend) = backend {
                match backend.apply_feedback(&member, &Feedback::PlayerLed { index: index.get() }) {
                    Ok(()) | Err(crate::BackendError::Unsupported) => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        Ok(())
    }
}
