#![forbid(unsafe_code)]

use polymer_bluetooth::manager::{COMMAND_RESPONSE_TIMEOUT, SendRaw, SetExternalOwnership};

pub mod airmouse;
pub mod decoder;

use decoder::{BluetoothControllerDevice, GamepadState, PointerSource};
use parking_lot::{Mutex, RwLock};
use polymer_bluetooth::{
    BluetoothManager, ConnectedController, ControllerId, ManagerConfig, ManagerEvent, SessionId,
    TransportKind,
};
use polymer_input::{
    BackendError, BackendHandle, BackendId, BatteryState, DeviceDescriptor, DeviceId, DeviceState,
    Feedback, InputBackend, PointerFlags, PointerKind, PointerReport, PointerSample, SensorReport,
};
use std::{collections::HashMap, sync::Arc};
use tokio::{sync::broadcast, task::JoinHandle};

#[derive(Clone, Debug)]
pub enum DiscoveryEvent {
    Manager(ManagerEvent),
    UnsupportedReportFormat {
        controller_id: ControllerId,
        profile_id: String,
    },
}

struct LiveController {
    device: DeviceId,
    session: SessionId,
    descriptor: DeviceDescriptor,
    controller: Box<dyn BluetoothControllerDevice>,
    previous_pointer: [i16; 2],
}

pub struct BluetoothBackend {
    manager: BluetoothManager,
    events: broadcast::Sender<DiscoveryEvent>,
    controllers: Arc<RwLock<HashMap<ControllerId, LiveController>>>,
    handle: Arc<RwLock<Option<BackendHandle>>>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl BluetoothBackend {
    pub async fn start(config: ManagerConfig) -> Result<Self, BackendError> {
        let manager = BluetoothManager::start(config)
            .await
            .map_err(|error| BackendError::Unavailable(error.to_string()))?;
        let (events, _) = broadcast::channel(128);
        Ok(Self {
            manager,
            events,
            controllers: Arc::new(RwLock::new(HashMap::new())),
            handle: Arc::new(RwLock::new(None)),
            task: Mutex::new(None),
        })
    }

    pub fn manager(&self) -> BluetoothManager {
        self.manager.clone()
    }

    pub fn discovery(&self) -> broadcast::Receiver<DiscoveryEvent> {
        self.events.subscribe()
    }

    pub fn descriptors(&self) -> Vec<DeviceDescriptor> {
        self.controllers
            .read()
            .values()
            .map(|controller| controller.descriptor.clone())
            .collect()
    }

    fn session_for(&self, device: &DeviceId) -> Option<SessionId> {
        self.controllers
            .read()
            .values()
            .find(|controller| &controller.device == device)
            .map(|controller| controller.session)
    }
}

impl InputBackend for BluetoothBackend {
    fn id(&self) -> BackendId {
        BackendId::BLUETOOTH
    }

    fn start(&self, handle: BackendHandle) -> Result<(), BackendError> {
        if self.task.lock().is_some() {
            return Err(BackendError::Failed(
                "Bluetooth backend is already running".to_owned(),
            ));
        }
        *self.handle.write() = Some(handle.clone());
        let manager_events = self.manager.events();
        let events = self.events.clone();
        let controllers = self.controllers.clone();
        let task = tokio::spawn(Self::run(manager_events, events, controllers, handle));
        *self.task.lock() = Some(task);
        Ok(())
    }

    fn shutdown(&self) {
        if let Some(task) = self.task.lock().take() {
            task.abort();
        }
        let manager = self.manager.clone();
        tokio::spawn(async move {
            let _ = manager
                .actor
                .send(polymer_bluetooth::manager::Shutdown)
                .await;
        });
    }

    fn apply_feedback(&self, device: &DeviceId, feedback: &Feedback) -> Result<(), BackendError> {
        let mut controllers = self.controllers.write();
        let controller = controllers
            .values_mut()
            .find(|controller| &controller.device == device)
            .ok_or_else(|| BackendError::Unavailable(device.to_string()))?;
        let session = controller.session;
        controller
            .controller
            .apply_feedback(&self.manager, session, feedback)
    }

    fn send_raw_hid(&self, device: &DeviceId, report: &[u8]) -> Result<(), BackendError> {
        let session = self
            .session_for(device)
            .ok_or_else(|| BackendError::Unavailable(device.to_string()))?;
        let manager = self.manager.clone();
        let report = report.to_vec();
        tokio::spawn(async move {
            let _ = manager
                .actor
                .send(SendRaw {
                    session_id: session,
                    bytes: report,
                })
                .timeout(COMMAND_RESPONSE_TIMEOUT)
                .await;
        });
        Ok(())
    }

    fn set_raw_hid_capture(&self, device: &DeviceId, enabled: bool) -> Result<(), BackendError> {
        let session = self
            .session_for(device)
            .ok_or_else(|| BackendError::Unavailable(device.to_string()))?;
        let manager = self.manager.clone();
        tokio::spawn(async move {
            let _ = manager
                .actor
                .send(SetExternalOwnership {
                    session_id: session,
                    enabled,
                })
                .timeout(COMMAND_RESPONSE_TIMEOUT)
                .await;
        });
        Ok(())
    }

    fn raw_hid_filter(&self, device: &DeviceId) -> Option<Arc<dyn polymer_input::RawHidFilter>> {
        self.controllers
            .read()
            .values()
            .find(|controller| &controller.device == device)
            .and_then(|controller| controller.controller.raw_hid_filter())
    }
}

impl BluetoothBackend {
    async fn run(
        mut manager_events: broadcast::Receiver<ManagerEvent>,
        events: broadcast::Sender<DiscoveryEvent>,
        controllers: Arc<RwLock<HashMap<ControllerId, LiveController>>>,
        handle: BackendHandle,
    ) {
        loop {
            let event = match manager_events.recv().await {
                Ok(event) => event,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            };
            match &event {
                ManagerEvent::Connected(connected) => {
                    Self::connect(connected, &events, &controllers, &handle);
                }
                ManagerEvent::RawReport {
                    controller_id,
                    session_id,
                    received_at,
                    bytes,
                } => {
                    let raw_device = controllers
                        .read()
                        .get(controller_id)
                        .and_then(|controller| {
                            (controller.session == *session_id).then(|| controller.device.clone())
                        });
                    let Some(raw_device) = raw_device else {
                        continue;
                    };
                    // Raw HID is the transport stream, not a by-product of normalized
                    // decoding. Dolphin needs status, acknowledgement, memory-reply,
                    // extension-only, and currently unknown reports as well as data modes.
                    handle.publish_raw_hid(&raw_device, bytes);

                    let published = {
                        let mut controllers = controllers.write();
                        let Some(controller) = controllers.get_mut(controller_id) else {
                            continue;
                        };
                        let Some(decoded) = controller.controller.decode(bytes, *received_at)
                        else {
                            continue;
                        };
                        controller.descriptor.role = decoded.role;
                        let input = decoded.state;
                        let (mut state, pointer) = Self::state(
                            input,
                            decoded.pointer_source,
                            decoded.motion,
                            decoded.battery_level,
                            decoded.charging,
                            controller.previous_pointer,
                        );
                        controller.previous_pointer = input.pointer;
                        controller.controller.apply_calibration(&mut state);
                        (controller.device.clone(), state, pointer)
                    };
                    handle.publish(&published.0, &published.1);
                    handle.publish_pointer(&published.0, &published.2);
                }
                ManagerEvent::Disconnected {
                    controller_id,
                    session_id,
                    ..
                } => {
                    let device = controllers
                        .read()
                        .get(controller_id)
                        .and_then(|controller| {
                            (controller.session == *session_id).then(|| controller.device.clone())
                        });
                    if let Some(device) = device {
                        handle.disconnected(&device);
                    }
                }
                _ => {}
            }
            let _ = events.send(DiscoveryEvent::Manager(event));
        }
    }

    fn connect(
        connected: &ConnectedController,
        events: &broadcast::Sender<DiscoveryEvent>,
        controllers: &RwLock<HashMap<ControllerId, LiveController>>,
        handle: &BackendHandle,
    ) {
        let controller = decoder::for_profile(&connected.profile_id);
        let supported = controller.is_some();
        let mut controller = controller.unwrap_or_else(decoder::unsupported);
        controller.set_ds3_motion(connected.ds3_motion.clone());
        controller.set_sensor_calibration(connected.sensor_calibration);
        let device = Self::device_id(connected.controller_id);
        let profile = connected.profile_id.as_str();
        if !supported {
            let _ = events.send(DiscoveryEvent::UnsupportedReportFormat {
                controller_id: connected.controller_id,
                profile_id: profile.to_owned(),
            });
        }
        let descriptor = controller.descriptor(device.clone(), connected);
        controllers.write().insert(
            connected.controller_id,
            LiveController {
                device,
                session: connected.session_id,
                descriptor: descriptor.clone(),
                controller,
                previous_pointer: [0; 2],
            },
        );
        handle.connected(descriptor);
    }

    fn state(
        input: GamepadState,
        source: Option<PointerSource>,
        motion: Option<decoder::MotionFrame>,
        battery_level: Option<u8>,
        charging: bool,
        previous_pointer: [i16; 2],
    ) -> (DeviceState, PointerSample) {
        let kind = match source {
            Some(PointerSource::IrCamera) => PointerKind::IrCamera,
            Some(PointerSource::AirMouse) => PointerKind::AirMouse,
            Some(PointerSource::Optical) => PointerKind::Optical,
            Some(PointerSource::Touchpad) => PointerKind::Touchpad,
            None => PointerKind::None,
        };
        let mut flags = match source {
            Some(PointerSource::Optical) => PointerFlags::NATIVE,
            Some(_) => PointerFlags::DEVICE | PointerFlags::NATIVE,
            None => PointerFlags::default(),
        };
        let contact = input
            .touch_contacts
            .is_some_and(|contacts| contacts.iter().any(|c| c.active));
        if source == Some(PointerSource::Touchpad) && contact {
            flags = flags | PointerFlags::CONTACT;
        }
        let delta = if source == Some(PointerSource::Optical) {
            input.pointer
        } else {
            std::array::from_fn(|axis| {
                (i32::from(input.pointer[axis]) - i32::from(previous_pointer[axis]))
                    .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
            })
        };
        let pointer = match source {
            Some(PointerSource::Touchpad) => PointerSample::Surface {
                position: Self::normalized(input.pointer),
                contact,
            },
            Some(PointerSource::Optical) => PointerSample::Relative {
                delta: [f32::from(delta[0]), f32::from(delta[1])],
                counts_per_unit: 4096.0,
            },
            Some(PointerSource::IrCamera) => PointerSample::Angular {
                position: Self::normalized(input.pointer),
                field_of_view: [33.0_f32.to_radians(), 23.0_f32.to_radians()],
                confidence: 1.0,
            },
            Some(PointerSource::AirMouse) => PointerSample::Angular {
                position: Self::normalized(input.pointer),
                field_of_view: [1.0, 1.0],
                confidence: 0.25,
            },
            None => PointerSample::None,
        };
        let mut sensor = SensorReport {
            accelerometer: input.accelerometer,
            gyroscope: input.gyroscope,
            ..SensorReport::default()
        };
        if let Some(motion) = motion {
            sensor.orientation = motion.orientation;
            sensor.has_orientation = true;
            sensor.sampled_at_ns = motion.orientation_sampled_at.elapsed().as_nanos() as u64;
        }
        (
            DeviceState {
                gamepad: polymer_input::GamepadReport {
                    buttons: input.buttons,
                    pressure: input.pressure,
                    left_stick: input.left_stick,
                    right_stick: input.right_stick,
                    left_trigger: input.left_trigger,
                    right_trigger: input.right_trigger,
                },
                sensor,
                pointer: PointerReport {
                    position: if source == Some(PointerSource::Optical) {
                        [0; 2]
                    } else {
                        input.pointer
                    },
                    delta,
                    flags,
                    kind,
                },
                battery: BatteryState {
                    level: battery_level,
                    charging,
                },
            },
            pointer,
        )
    }

    fn normalized(pointer: [i16; 2]) -> [f32; 2] {
        [
            (f32::from(pointer[0]) + 32768.0) / 65535.0,
            (f32::from(pointer[1]) + 32768.0) / 65535.0,
        ]
    }

    fn device_id(controller: ControllerId) -> DeviceId {
        let transport = match controller.transport {
            TransportKind::Ble => "ble",
            TransportKind::BrEdr => "br-edr",
        };
        DeviceId::new(
            BackendId::BLUETOOTH,
            &format!("{transport}/{}", controller.address),
        )
    }
}

impl Drop for BluetoothBackend {
    fn drop(&mut self) {
        if let Some(task) = self.task.get_mut().take() {
            task.abort();
        }
    }
}
