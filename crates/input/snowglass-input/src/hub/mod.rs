mod assignment;
mod calibration;
mod registry;

pub use calibration::CalibrationCapture;
use calibration::CalibrationSession;

use crate::{
    BackendError, BackendHandle, BackendId, Calibration, CalibrationError, CalibrationKind,
    DeviceDescriptor, DeviceId, DeviceState, Feedback, HdRumbleFormat, HdRumbleStream,
    HostInputEvent, HostListener, HostListenerHandle, InputBackend, InputEvent, Listener,
    ListenerHandle, ListenerId, PlayerIndex, PointerReport, PointerSample,
};
use dashmap::{DashMap, DashSet};
use parking_lot::RwLock;
use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompositionMode {
    ByRole,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AssignmentPolicy {
    pub max_players: u8,
    pub auto_assign: bool,
    pub auto_compose: bool,
    pub retain_disconnected: bool,
}

impl Default for AssignmentPolicy {
    fn default() -> Self {
        Self {
            max_players: 8,
            auto_assign: true,
            auto_compose: true,
            retain_disconnected: true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HubError {
    DuplicateBackend(BackendId),
    BackendUnavailable(BackendId),
    DeviceNotFound(DeviceId),
    DeviceUnavailable(DeviceId),
    InvalidDeviceBackend,
    InvalidPlayerIndex(PlayerIndex),
    AlreadyComposed(DeviceId),
    InvalidComposition,
    ListenerNotFound(ListenerId),
    FeedbackNotOwned,
    HubOwned,
    Unsupported,
    Backend(BackendError),
    Calibration(CalibrationError),
}

impl fmt::Display for HubError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for HubError {}

impl From<BackendError> for HubError {
    fn from(value: BackendError) -> Self {
        match value {
            BackendError::Unsupported => Self::Unsupported,
            value => Self::Backend(value),
        }
    }
}

#[derive(Clone)]
pub struct InputHub {
    pub(crate) inner: Arc<HubInner>,
}

pub(crate) struct HubInner {
    pub(crate) topology: RwLock<()>,
    pub(crate) policy: RwLock<AssignmentPolicy>,
    pub(crate) backends: DashMap<BackendId, Arc<dyn InputBackend>>,
    pub(crate) devices: DashMap<DeviceId, DeviceEntry>,
    pub(crate) assignments: DashMap<PlayerIndex, DeviceId>,
    pub(crate) composites: DashMap<DeviceId, Composite>,
    pub(crate) decomposed: DashSet<DeviceId>,
    pub(crate) individual_devices: DashSet<DeviceId>,
    pub(crate) sequences: DashMap<PlayerIndex, u32>,
    pub(crate) pointer_overrides: DashMap<PlayerIndex, PointerReport>,
    pub(crate) listeners: DashMap<ListenerId, Listener>,
    pub(crate) host_listeners: DashMap<ListenerId, HostListener>,
    pub(crate) feedback_priority: RwLock<Option<ListenerId>>,
    pub(crate) next_listener: AtomicU64,
    pub(crate) home_led: DashMap<DeviceId, u8>,
    pub(crate) calibration_sessions: DashMap<DeviceId, CalibrationSession>,
    pub(crate) next_pointer_revision: AtomicU64,
    pub(crate) composition_revision: AtomicU64,
}

#[derive(Clone)]
pub(crate) struct DeviceEntry {
    pub(crate) descriptor: DeviceDescriptor,
    pub(crate) state: DeviceState,
    pub(crate) pointer: PointerSample,
    pub(crate) pointer_revision: u64,
    pub(crate) connected: bool,
}

#[derive(Clone)]
pub(crate) struct Composite {
    pub(crate) members: Vec<DeviceId>,
    pub(crate) mode: CompositionMode,
}

impl InputHub {
    pub fn new(policy: AssignmentPolicy) -> Self {
        Self {
            inner: Arc::new(HubInner {
                topology: RwLock::new(()),
                policy: RwLock::new(policy),
                backends: DashMap::new(),
                devices: DashMap::new(),
                assignments: DashMap::new(),
                composites: DashMap::new(),
                decomposed: DashSet::new(),
                individual_devices: DashSet::new(),
                sequences: DashMap::new(),
                pointer_overrides: DashMap::new(),
                listeners: DashMap::new(),
                host_listeners: DashMap::new(),
                feedback_priority: RwLock::new(None),
                next_listener: AtomicU64::new(1),
                home_led: DashMap::new(),
                calibration_sessions: DashMap::new(),
                next_pointer_revision: AtomicU64::new(1),
                composition_revision: AtomicU64::new(0),
            }),
        }
    }

    pub fn register_backend(&self, backend: Arc<dyn InputBackend>) -> Result<(), HubError> {
        let id = backend.id();
        {
            let _topology = self.inner.topology.write();
            if self.inner.backends.contains_key(&id) {
                return Err(HubError::DuplicateBackend(id));
            }
            self.inner.backends.insert(id, backend.clone());
        }
        if let Err(error) = backend.start(BackendHandle::new(&self.inner, id)) {
            self.inner.backends.remove(&id);
            return Err(error.into());
        }
        Ok(())
    }

    pub fn add_listener(&self, listener: Arc<dyn crate::InputListener>) -> ListenerHandle {
        let (id, devices, states) = {
            let _topology = self.inner.topology.write();
            let id = self.inner.next_listener_id();
            self.inner.listeners.insert(id, listener.clone());
            let mut devices = self
                .inner
                .devices
                .iter()
                .map(|entry| {
                    (
                        self.inner.index_of(&entry.descriptor.id),
                        entry.descriptor.clone(),
                        entry.connected,
                    )
                })
                .collect::<Vec<_>>();
            devices.sort_by(|(_, left, _), (_, right, _)| left.id.cmp(&right.id));
            let mut states = self
                .inner
                .assignments
                .iter()
                .filter_map(|assignment| {
                    let index = *assignment.key();
                    let device = assignment.value().clone();
                    let entry = self.inner.devices.get(&device)?;
                    entry.connected.then_some((
                        index,
                        self.inner.sequences.get(&index).map_or(0, |value| *value),
                        device,
                        entry.state,
                    ))
                })
                .collect::<Vec<_>>();
            states.sort_by_key(|(index, _, _, _)| *index);
            (id, devices, states)
        };
        for (index, device, connected) in &devices {
            if *connected {
                listener.on_input(InputEvent::Connected {
                    index: *index,
                    device,
                });
            }
        }
        for (index, sequence, device, state) in &states {
            listener.on_input(InputEvent::State {
                index: *index,
                sequence: *sequence,
                device,
                state,
            });
        }
        ListenerHandle {
            hub: self.clone(),
            id,
            attached: true,
        }
    }

    pub fn add_host_listener(
        &self,
        listener: Arc<dyn crate::HostInputListener>,
    ) -> HostListenerHandle {
        let id = self.inner.next_listener_id();
        self.inner.host_listeners.insert(id, listener);
        HostListenerHandle {
            hub: self.clone(),
            id,
            attached: true,
        }
    }

    pub fn publish_host(&self, event: HostInputEvent) {
        let mut listeners = self
            .inner
            .host_listeners
            .iter()
            .map(|entry| (*entry.key(), entry.value().clone()))
            .collect::<Vec<_>>();
        listeners.sort_by_key(|(id, _)| *id);
        for (_, listener) in listeners {
            listener.on_host_input(&event);
        }
    }

    pub fn assign(&self, device: &DeviceId, index: Option<PlayerIndex>) -> Result<(), HubError> {
        self.inner.assign(device, index, true)
    }

    pub fn assignments(&self) -> Vec<(PlayerIndex, DeviceId)> {
        let _topology = self.inner.topology.read();
        let mut assignments = self
            .inner
            .assignments
            .iter()
            .map(|entry| (*entry.key(), entry.value().clone()))
            .collect::<Vec<_>>();
        assignments.sort_by_key(|(index, _)| *index);
        assignments
    }

    pub fn set_auto_assign(&self, enabled: bool) {
        self.inner.policy.write().auto_assign = enabled;
    }

    pub fn devices(&self) -> Vec<(Option<PlayerIndex>, DeviceDescriptor)> {
        let _topology = self.inner.topology.read();
        let mut devices = self
            .inner
            .devices
            .iter()
            .map(|entry| {
                (
                    self.inner.index_of(&entry.descriptor.id),
                    entry.descriptor.clone(),
                )
            })
            .collect::<Vec<_>>();
        devices.sort_by(|(_, left), (_, right)| left.id.cmp(&right.id));
        devices
    }

    pub fn snapshot(&self) -> Vec<(Option<PlayerIndex>, DeviceDescriptor, DeviceState)> {
        let _topology = self.inner.topology.read();
        let mut devices = self
            .inner
            .devices
            .iter()
            .map(|entry| {
                (
                    self.inner.index_of(&entry.descriptor.id),
                    entry.descriptor.clone(),
                    entry.state,
                )
            })
            .collect::<Vec<_>>();
        devices.sort_by(|(_, left, _), (_, right, _)| left.id.cmp(&right.id));
        devices
    }

    pub fn device_snapshots(
        &self,
    ) -> Vec<(Option<PlayerIndex>, DeviceDescriptor, DeviceState, bool)> {
        let _topology = self.inner.topology.read();
        let mut devices = self
            .inner
            .devices
            .iter()
            .map(|entry| {
                (
                    self.inner.index_of(&entry.descriptor.id),
                    entry.descriptor.clone(),
                    entry.state,
                    entry.connected,
                )
            })
            .collect::<Vec<_>>();
        devices.sort_by(|(_, left, _, _), (_, right, _, _)| left.id.cmp(&right.id));
        devices
    }

    pub fn state(&self, index: PlayerIndex) -> Option<DeviceState> {
        let _topology = self.inner.topology.read();
        let device = self.inner.assignments.get(&index)?.value().clone();
        let mut value = self.inner.devices.get(&device)?.state;
        if let Some(pointer) = self.inner.pointer_overrides.get(&index) {
            value.pointer = *pointer;
        }
        Some(value)
    }

    pub fn pointer_sample(&self, index: PlayerIndex) -> Option<PointerSample> {
        self.pointer_snapshot(index).map(|(_, sample)| sample)
    }

    pub fn pointer_snapshot(&self, index: PlayerIndex) -> Option<(u64, PointerSample)> {
        let _topology = self.inner.topology.read();
        let device = self.inner.assignments.get(&index)?.value().clone();
        let entry = self.inner.devices.get(&device)?;
        Some((entry.pointer_revision, entry.pointer))
    }

    pub fn raw_hid_filter(&self, index: PlayerIndex) -> Option<Arc<dyn crate::RawHidFilter>> {
        let _topology = self.inner.topology.read();
        let device = self.inner.assignments.get(&index)?.value().clone();
        let backend = self.inner.backends.get(&device.backend())?;
        backend.raw_hid_filter(&device)
    }

    pub fn set_pointer_override(&self, index: PlayerIndex, pointer: Option<PointerReport>) {
        if let Some(pointer) = pointer {
            self.inner.pointer_overrides.insert(index, pointer);
        } else {
            self.inner.pointer_overrides.remove(&index);
        }
    }

    pub fn compose(
        &self,
        members: &[DeviceId],
        mode: CompositionMode,
    ) -> Result<DeviceId, HubError> {
        self.inner.compose(members, mode, false)
    }

    pub fn decompose(&self, composite: &DeviceId) -> Result<(), HubError> {
        self.inner.decompose(composite)
    }

    pub fn set_feedback_priority(&self, listener: Option<ListenerId>) {
        *self.inner.feedback_priority.write() = listener;
    }

    pub fn set_max_players(&self, max: u8) {
        let _topology = self.inner.topology.write();
        let max_players = max.min(8);
        self.inner.policy.write().max_players = max_players;
        self.inner
            .assignments
            .retain(|index, _| index.get() < max_players);
    }

    pub(crate) fn remove_listener(&self, id: ListenerId) {
        self.inner.listeners.remove(&id);
        let mut priority = self.inner.feedback_priority.write();
        if *priority == Some(id) {
            *priority = None;
        }
    }

    pub(crate) fn remove_host_listener(&self, id: ListenerId) {
        self.inner.host_listeners.remove(&id);
    }

    pub(crate) fn feedback(
        &self,
        listener: ListenerId,
        index: PlayerIndex,
        feedback: Feedback,
    ) -> Result<(), HubError> {
        self.inner.dispatch_feedback(listener, index, feedback)
    }

    pub(crate) fn open_hd_rumble(
        &self,
        listener: ListenerId,
        index: PlayerIndex,
        format: HdRumbleFormat,
    ) -> Result<HdRumbleStream, HubError> {
        let (device, backend) = {
            let _topology = self.inner.topology.read();
            let priority = *self.inner.feedback_priority.read();
            if priority.is_some() && priority != Some(listener) {
                return Err(HubError::FeedbackNotOwned);
            }
            let device = self
                .inner
                .assignments
                .get(&index)
                .ok_or(HubError::InvalidPlayerIndex(index))?
                .value()
                .clone();
            if self.inner.composites.contains_key(&device) {
                return Err(HubError::Unsupported);
            }
            let backend = self
                .inner
                .backends
                .get(&device.backend())
                .ok_or(HubError::BackendUnavailable(device.backend()))?
                .clone();
            (device, backend)
        };
        let sink = backend.open_hd_rumble(&device, format)?;
        Ok(HdRumbleStream::new(device, sink))
    }

    pub(crate) fn send_raw_hid(&self, index: PlayerIndex, report: &[u8]) -> Result<(), HubError> {
        self.inner.for_each_device(index, |backend, device| {
            backend.send_raw_hid(device, report)
        })
    }

    pub(crate) fn set_raw_hid_capture(
        &self,
        index: PlayerIndex,
        enabled: bool,
    ) -> Result<(), HubError> {
        self.inner.for_each_device(index, |backend, device| {
            backend.set_raw_hid_capture(device, enabled)
        })
    }

    pub fn calibration(&self, device: &DeviceId) -> Calibration {
        self.inner
            .calibration_sessions
            .get(device)
            .map(|session| session.calibration())
            .unwrap_or_default()
    }

    pub fn calibrate(&self, device: &DeviceId, calibration: &Calibration) -> Result<(), HubError> {
        self.inner.calibrate(device, calibration)
    }

    pub fn reset_calibration(&self, device: &DeviceId) -> Result<(), HubError> {
        self.inner.set_calibration(device, Calibration::default())
    }

    pub fn begin_calibration(
        &self,
        device: &DeviceId,
        kind: CalibrationKind,
    ) -> Result<CalibrationCapture, HubError> {
        let feedback = {
            let _topology = self.inner.topology.read();
            if !self.inner.devices.contains_key(device) {
                return Err(HubError::DeviceNotFound(device.clone()));
            }
            self.inner
                .calibration_sessions
                .entry(device.clone())
                .or_default()
                .begin(kind)
                .map_err(HubError::Calibration)?
        };
        if let Err(error) = self.inner.apply_feedback(device, &feedback) {
            if let Some(mut session) = self.inner.calibration_sessions.get_mut(device) {
                session.cancel();
            }
            return Err(error);
        }
        Ok(CalibrationCapture::new(self.clone(), device.clone()))
    }
}

impl Default for InputHub {
    fn default() -> Self {
        Self::new(AssignmentPolicy::default())
    }
}

impl HubInner {
    fn next_listener_id(&self) -> ListenerId {
        ListenerId(
            self.next_listener
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                    Some(value.wrapping_add(1).max(1))
                })
                .expect("listener identifiers always advance"),
        )
    }

    pub(crate) fn index_of(&self, device: &DeviceId) -> Option<PlayerIndex> {
        self.assignments
            .iter()
            .find_map(|entry| (entry.value() == device).then_some(*entry.key()))
    }

    pub(crate) fn lowest_free(&self) -> Option<PlayerIndex> {
        (0..self.policy.read().max_players)
            .map(PlayerIndex::new)
            .find(|index| !self.assignments.contains_key(index))
    }

    pub(crate) fn listeners(&self) -> Vec<Listener> {
        let mut listeners = self
            .listeners
            .iter()
            .map(|entry| (*entry.key(), entry.value().clone()))
            .collect::<Vec<_>>();
        listeners.sort_by_key(|(id, _)| *id);
        listeners
            .into_iter()
            .map(|(_, listener)| listener)
            .collect()
    }
}

impl Drop for HubInner {
    fn drop(&mut self) {
        for backend in &self.backends {
            backend.shutdown();
        }
    }
}
