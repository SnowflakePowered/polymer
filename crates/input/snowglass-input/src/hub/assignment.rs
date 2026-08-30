use super::{Composite, CompositionMode, DeviceEntry, HubError, HubInner, HubState};
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
            let mut state = self.state.lock();
            if !state.devices.contains_key(device) {
                return Err(HubError::DeviceNotFound(device.clone()));
            }
            if index.is_some_and(|value| value.get() >= state.policy.max_players) {
                return Err(HubError::InvalidPlayerIndex(index.unwrap()));
            }
            let from = state.index_of(device);
            if from == index {
                return Ok(());
            }
            if let Some(from) = from {
                state.assignments.remove(&from);
            }
            let mut events = vec![(device.clone(), from, index)];
            if let Some(index) = index {
                if let Some(displaced) = state.assignments.insert(index, device.clone()) {
                    let to = explicit.then(|| state.lowest_free()).flatten();
                    if let Some(to) = to {
                        state.assignments.insert(to, displaced.clone());
                    }
                    events.push((displaced, Some(index), to));
                }
            }
            events
        };
        let listeners = self
            .state
            .lock()
            .listeners
            .values()
            .cloned()
            .collect::<Vec<_>>();
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
            let mut state = self.state.lock();
            if state.decomposed.contains(&composite) && automatic {
                return Err(HubError::InvalidComposition);
            }
            if state.composites.contains_key(&composite) {
                return Ok(composite);
            }
            let entries = members
                .iter()
                .map(|member| {
                    if state
                        .composites
                        .values()
                        .any(|value| value.members.contains(member))
                    {
                        return Err(HubError::AlreadyComposed(member.clone()));
                    }
                    state
                        .devices
                        .get(member)
                        .cloned()
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
                if let Some(index) = state.index_of(member) {
                    state.assignments.remove(&index);
                }
            }
            state.devices.insert(
                composite.clone(),
                DeviceEntry {
                    descriptor: descriptor.clone(),
                    state: state_value,
                    pointer: Self::composite_pointer(&entries),
                    connected,
                },
            );
            state.composites.insert(
                composite.clone(),
                Composite {
                    members: members.to_vec(),
                    mode,
                },
            );
            descriptor
        };
        let listeners = self
            .state
            .lock()
            .listeners
            .values()
            .cloned()
            .collect::<Vec<_>>();
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
            let mut state = self.state.lock();
            let value = state
                .composites
                .remove(composite)
                .ok_or_else(|| HubError::DeviceNotFound(composite.clone()))?;
            let index = state.index_of(composite);
            if let Some(index) = index {
                state.assignments.remove(&index);
            }
            state.devices.remove(composite);
            state.decomposed.insert(composite.clone());
            (
                value.members,
                index,
                state.listeners.values().cloned().collect::<Vec<_>>(),
            )
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
            let state = self.state.lock();
            if !state.policy.auto_assign || state.index_of(device).is_some() {
                return;
            }
            let Some(entry) = state.devices.get(device) else {
                return;
            };
            if !entry.connected
                || (state.policy.auto_compose
                    && matches!(
                        entry.descriptor.role,
                        DeviceRole::LeftHalf | DeviceRole::RightHalf
                    ))
            {
                return;
            }
            state.lowest_free()
        };
        if let Some(index) = index {
            let _ = self.assign(device, Some(index), false);
        }
    }

    pub(crate) fn try_auto_compose(&self) {
        let members = {
            let state = self.state.lock();
            if !state.policy.auto_compose {
                return;
            }
            let eligible = |role| {
                state
                    .devices
                    .values()
                    .filter(|entry| {
                        entry.connected
                            && entry.descriptor.id.backend() == BackendId::BLUETOOTH
                            && entry.descriptor.role == role
                            && state.index_of(&entry.descriptor.id).is_none()
                            && !state
                                .composites
                                .values()
                                .any(|value| value.members.contains(&entry.descriptor.id))
                    })
                    .map(|entry| entry.descriptor.id.clone())
                    .collect::<Vec<_>>()
            };
            let left = eligible(DeviceRole::LeftHalf);
            let right = eligible(DeviceRole::RightHalf);
            if left.len() == 1 && right.len() == 1 {
                Some(vec![left[0].clone(), right[0].clone()])
            } else {
                None
            }
        };
        if let Some(members) = members {
            let _ = self.compose(&members, CompositionMode::ByRole, true);
        }
    }

    pub(crate) fn update_composites(&self, member: &DeviceId) {
        let composites = {
            let state = self.state.lock();
            state
                .composites
                .iter()
                .filter(|(_, value)| value.members.contains(member))
                .map(|(id, value)| (id.clone(), value.clone()))
                .collect::<Vec<_>>()
        };
        for (id, composite) in composites {
            self.update_composite(&id, &composite);
        }
    }

    fn update_composite(&self, id: &DeviceId, composite: &Composite) {
        let event = {
            let mut state = self.state.lock();
            let entries = composite
                .members
                .iter()
                .filter_map(|member| state.devices.get(member).cloned())
                .collect::<Vec<_>>();
            if entries.len() != composite.members.len() {
                return;
            }
            let connected = entries.iter().all(|entry| entry.connected);
            let value = Self::composite_state(&entries, composite.mode);
            let pointer = Self::composite_pointer(&entries);
            let Some(entry) = state.devices.get_mut(id) else {
                return;
            };
            let changed_connection = entry.connected != connected;
            entry.connected = connected;
            entry.state = value;
            entry.pointer = pointer;
            let index = state.index_of(id);
            let sequence = index.map(|index| {
                let sequence = state.sequences.entry(index).or_default();
                *sequence = sequence.wrapping_add(1);
                *sequence
            });
            (
                state.listeners.values().cloned().collect::<Vec<_>>(),
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
                    let descriptor = self
                        .state
                        .lock()
                        .devices
                        .get(id)
                        .unwrap()
                        .descriptor
                        .clone();
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
        let members = {
            let state = self.state.lock();
            state
                .composites
                .get(device)
                .map_or_else(|| vec![device.clone()], |value| value.members.clone())
        };
        for member in members {
            let backend = {
                let state = self.state.lock();
                state.backends.get(&member.backend()).cloned()
            };
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

impl HubState {
    pub(crate) fn lowest_free(&self) -> Option<PlayerIndex> {
        (0..self.policy.max_players)
            .map(PlayerIndex::new)
            .find(|index| !self.assignments.contains_key(index))
    }
}
