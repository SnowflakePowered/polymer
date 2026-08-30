mod assignment;
mod registry;

use crate::{
    BackendError, BackendHandle, BackendId, Calibration, CalibrationError, CalibrationKind,
    DeviceDescriptor, DeviceId, DeviceState, Feedback, HdRumbleFormat, HdRumbleStream,
    HostInputEvent, HostListener, HostListenerHandle, InputBackend, InputEvent, Listener,
    ListenerHandle, ListenerId, PlayerIndex, PointerReport, PointerSample,
};
use parking_lot::Mutex;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
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
    pub(crate) state: Mutex<HubState>,
}

pub(crate) struct HubState {
    pub(crate) policy: AssignmentPolicy,
    pub(crate) backends: BTreeMap<BackendId, Arc<dyn InputBackend>>,
    pub(crate) devices: BTreeMap<DeviceId, DeviceEntry>,
    pub(crate) assignments: BTreeMap<PlayerIndex, DeviceId>,
    pub(crate) composites: BTreeMap<DeviceId, Composite>,
    pub(crate) decomposed: BTreeSet<DeviceId>,
    pub(crate) sequences: BTreeMap<PlayerIndex, u32>,
    pub(crate) pointer_overrides: BTreeMap<PlayerIndex, PointerReport>,
    pub(crate) listeners: BTreeMap<ListenerId, Listener>,
    pub(crate) host_listeners: BTreeMap<ListenerId, HostListener>,
    pub(crate) feedback_priority: Option<ListenerId>,
    pub(crate) next_listener: u64,
    pub(crate) calibrations: BTreeMap<DeviceId, Calibration>,
    pub(crate) home_led: BTreeMap<DeviceId, u8>,
    pub(crate) captures: BTreeMap<DeviceId, CaptureState>,
}

#[derive(Clone)]
pub(crate) struct DeviceEntry {
    pub(crate) descriptor: DeviceDescriptor,
    pub(crate) state: DeviceState,
    pub(crate) pointer: PointerSample,
    pub(crate) connected: bool,
}

#[derive(Clone)]
pub(crate) struct Composite {
    pub(crate) members: Vec<DeviceId>,
    pub(crate) mode: CompositionMode,
}

pub(crate) struct CaptureState {
    pub(crate) kind: CalibrationKind,
    pub(crate) samples: u32,
    pub(crate) stick_sum: [[i64; 2]; 2],
    pub(crate) stick_min: [[i16; 2]; 2],
    pub(crate) stick_max: [[i16; 2]; 2],
    pub(crate) trigger_min: [u16; 2],
    pub(crate) trigger_max: [u16; 2],
    pub(crate) gyro_sum: [f64; 3],
}

impl CaptureState {
    fn new(kind: CalibrationKind) -> Self {
        Self {
            kind,
            samples: 0,
            stick_sum: [[0; 2]; 2],
            stick_min: [[i16::MAX; 2]; 2],
            stick_max: [[i16::MIN; 2]; 2],
            trigger_min: [u16::MAX; 2],
            trigger_max: [u16::MIN; 2],
            gyro_sum: [0.0; 3],
        }
    }

    pub(crate) fn accept(&mut self, state: &DeviceState) {
        self.samples = self.samples.saturating_add(1);
        let sticks = [state.gamepad.left_stick, state.gamepad.right_stick];
        for stick in 0..2 {
            for axis in 0..2 {
                let value = sticks[stick][axis];
                self.stick_sum[stick][axis] += i64::from(value);
                self.stick_min[stick][axis] = self.stick_min[stick][axis].min(value);
                self.stick_max[stick][axis] = self.stick_max[stick][axis].max(value);
            }
        }
        let triggers = [state.gamepad.left_trigger, state.gamepad.right_trigger];
        for trigger in 0..2 {
            self.trigger_min[trigger] = self.trigger_min[trigger].min(triggers[trigger]);
            self.trigger_max[trigger] = self.trigger_max[trigger].max(triggers[trigger]);
        }
        for axis in 0..3 {
            self.gyro_sum[axis] += f64::from(state.sensor.gyroscope[axis]);
        }
    }
}

impl InputHub {
    pub fn new(policy: AssignmentPolicy) -> Self {
        Self {
            inner: Arc::new(HubInner {
                state: Mutex::new(HubState {
                    policy,
                    backends: BTreeMap::new(),
                    devices: BTreeMap::new(),
                    assignments: BTreeMap::new(),
                    composites: BTreeMap::new(),
                    decomposed: BTreeSet::new(),
                    sequences: BTreeMap::new(),
                    pointer_overrides: BTreeMap::new(),
                    listeners: BTreeMap::new(),
                    host_listeners: BTreeMap::new(),
                    feedback_priority: None,
                    next_listener: 1,
                    calibrations: BTreeMap::new(),
                    home_led: BTreeMap::new(),
                    captures: BTreeMap::new(),
                }),
            }),
        }
    }

    pub fn register_backend(&self, backend: Arc<dyn InputBackend>) -> Result<(), HubError> {
        let id = backend.id();
        {
            let mut state = self.inner.state.lock();
            if state.backends.contains_key(&id) {
                return Err(HubError::DuplicateBackend(id));
            }
            state.backends.insert(id, backend.clone());
        }
        if let Err(error) = backend.start(BackendHandle::new(&self.inner, id)) {
            self.inner.state.lock().backends.remove(&id);
            return Err(error.into());
        }
        Ok(())
    }

    pub fn add_listener(&self, listener: Arc<dyn crate::InputListener>) -> ListenerHandle {
        let (id, devices, states) = {
            let mut state = self.inner.state.lock();
            let id = ListenerId(state.next_listener);
            state.next_listener = state.next_listener.wrapping_add(1).max(1);
            state.listeners.insert(id, listener.clone());
            let devices = state
                .devices
                .values()
                .map(|entry| {
                    (
                        state.index_of(&entry.descriptor.id),
                        entry.descriptor.clone(),
                        entry.connected,
                    )
                })
                .collect::<Vec<_>>();
            let states = state
                .assignments
                .iter()
                .filter_map(|(index, device)| {
                    let entry = state.devices.get(device)?;
                    entry.connected.then_some((
                        *index,
                        *state.sequences.get(index).unwrap_or(&0),
                        device.clone(),
                        entry.state,
                    ))
                })
                .collect::<Vec<_>>();
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
        let id = {
            let mut state = self.inner.state.lock();
            let id = ListenerId(state.next_listener);
            state.next_listener = state.next_listener.wrapping_add(1).max(1);
            state.host_listeners.insert(id, listener);
            id
        };
        HostListenerHandle {
            hub: self.clone(),
            id,
            attached: true,
        }
    }

    pub fn publish_host(&self, event: HostInputEvent) {
        let listeners = self
            .inner
            .state
            .lock()
            .host_listeners
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for listener in listeners {
            listener.on_host_input(&event);
        }
    }

    pub fn assign(&self, device: &DeviceId, index: Option<PlayerIndex>) -> Result<(), HubError> {
        self.inner.assign(device, index, true)
    }

    pub fn assignments(&self) -> Vec<(PlayerIndex, DeviceId)> {
        self.inner
            .state
            .lock()
            .assignments
            .iter()
            .map(|(index, device)| (*index, device.clone()))
            .collect()
    }

    pub fn devices(&self) -> Vec<(Option<PlayerIndex>, DeviceDescriptor)> {
        let state = self.inner.state.lock();
        state
            .devices
            .values()
            .map(|entry| {
                (
                    state.index_of(&entry.descriptor.id),
                    entry.descriptor.clone(),
                )
            })
            .collect()
    }

    pub fn snapshot(&self) -> Vec<(Option<PlayerIndex>, DeviceDescriptor, DeviceState)> {
        let state = self.inner.state.lock();
        state
            .devices
            .values()
            .map(|entry| {
                (
                    state.index_of(&entry.descriptor.id),
                    entry.descriptor.clone(),
                    entry.state,
                )
            })
            .collect()
    }

    pub fn state(&self, index: PlayerIndex) -> Option<DeviceState> {
        let state = self.inner.state.lock();
        let device = state.assignments.get(&index)?;
        let mut value = state.devices.get(device)?.state;
        if let Some(pointer) = state.pointer_overrides.get(&index) {
            value.pointer = *pointer;
        }
        Some(value)
    }

    pub fn pointer_sample(&self, index: PlayerIndex) -> Option<PointerSample> {
        let state = self.inner.state.lock();
        let device = state.assignments.get(&index)?;
        Some(state.devices.get(device)?.pointer)
    }

    pub fn raw_hid_filter(&self, index: PlayerIndex) -> Option<Arc<dyn crate::RawHidFilter>> {
        let state = self.inner.state.lock();
        let device = state.assignments.get(&index)?;
        let backend = state.backends.get(&device.backend())?;
        backend.raw_hid_filter(device)
    }

    pub fn set_pointer_override(&self, index: PlayerIndex, pointer: Option<PointerReport>) {
        let mut state = self.inner.state.lock();
        if let Some(pointer) = pointer {
            state.pointer_overrides.insert(index, pointer);
        } else {
            state.pointer_overrides.remove(&index);
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
        self.inner.state.lock().feedback_priority = listener;
    }

    pub fn set_max_players(&self, max: u8) {
        let mut state = self.inner.state.lock();
        state.policy.max_players = max.min(8);
        let max_players = state.policy.max_players;
        state
            .assignments
            .retain(|index, _| index.get() < max_players);
    }

    pub(crate) fn remove_listener(&self, id: ListenerId) {
        let mut state = self.inner.state.lock();
        state.listeners.remove(&id);
        if state.feedback_priority == Some(id) {
            state.feedback_priority = None;
        }
    }

    pub(crate) fn remove_host_listener(&self, id: ListenerId) {
        self.inner.state.lock().host_listeners.remove(&id);
    }

    pub(crate) fn feedback(
        &self,
        listener: ListenerId,
        index: PlayerIndex,
        feedback: Feedback,
    ) -> Result<(), HubError> {
        self.inner.feedback(listener, index, feedback)
    }

    pub(crate) fn open_hd_rumble(
        &self,
        listener: ListenerId,
        index: PlayerIndex,
        format: HdRumbleFormat,
    ) -> Result<HdRumbleStream, HubError> {
        let (device, backend) = {
            let state = self.inner.state.lock();
            if state.feedback_priority.is_some() && state.feedback_priority != Some(listener) {
                return Err(HubError::FeedbackNotOwned);
            }
            let device = state
                .assignments
                .get(&index)
                .ok_or(HubError::InvalidPlayerIndex(index))?
                .clone();
            if state.composites.contains_key(&device) {
                return Err(HubError::Unsupported);
            }
            let backend = state
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
            .state
            .lock()
            .calibrations
            .get(device)
            .cloned()
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
        let calibration = {
            let mut state = self.inner.state.lock();
            if !state.devices.contains_key(device) {
                return Err(HubError::DeviceNotFound(device.clone()));
            }
            if state.captures.contains_key(device) {
                return Err(HubError::Calibration(CalibrationError::Cancelled));
            }
            state
                .captures
                .insert(device.clone(), CaptureState::new(kind));
            state.calibrations.get(device).cloned().unwrap_or_default()
        };
        self.inner
            .apply_calibration(device, &calibration.without_shaping())?;
        Ok(CalibrationCapture {
            hub: self.clone(),
            device: device.clone(),
            active: AtomicBool::new(true),
        })
    }
}

impl Default for InputHub {
    fn default() -> Self {
        Self::new(AssignmentPolicy::default())
    }
}

pub struct CalibrationCapture {
    hub: InputHub,
    device: DeviceId,
    active: AtomicBool,
}

impl CalibrationCapture {
    pub fn progress(&self) -> f32 {
        let state = self.hub.inner.state.lock();
        let samples = state
            .captures
            .get(&self.device)
            .map_or(0, |capture| capture.samples);
        (samples as f32 / 120.0).clamp(0.0, 1.0)
    }

    pub fn finish(self) -> Result<(), CalibrationError> {
        self.active.store(false, Ordering::Release);
        self.hub.inner.finish_calibration(&self.device)
    }

    pub fn cancel(self) {
        self.active.store(false, Ordering::Release);
        self.hub.inner.cancel_calibration(&self.device);
    }
}

impl Drop for CalibrationCapture {
    fn drop(&mut self) {
        if self.active.swap(false, Ordering::AcqRel) {
            self.hub.inner.cancel_calibration(&self.device);
        }
    }
}

impl HubState {
    pub(crate) fn index_of(&self, device: &DeviceId) -> Option<PlayerIndex> {
        self.assignments
            .iter()
            .find_map(|(index, candidate)| (candidate == device).then_some(*index))
    }
}

impl Drop for HubInner {
    fn drop(&mut self) {
        for backend in self.state.get_mut().backends.values() {
            backend.shutdown();
        }
    }
}
