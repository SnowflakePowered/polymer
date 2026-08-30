//! Nintendo Bluetooth input provider layered above `snowglass-bluetooth`.
//!
//! The Bluetooth crate owns radios and controller lifecycle. This crate owns
//! report decoding, motion units, pointer synthesis, and virtual-slot routing.

#![forbid(unsafe_code)]

pub mod airmouse;
pub mod decoder;
#[cfg(windows)]
pub mod raw;

use decoder::{MotionFrame, PointerSource, ReportDecoder};
use parking_lot::RwLock;
use snowglass_bluetooth::{
    BluetoothManager, ConnectedController, ControllerId, ManagerConfig, ManagerEvent, Result,
    SessionId, TransportKind,
};
use snowglass_input::{
    gamepad::{GamepadFeedback, GamepadState},
    router::{
        PhysicalControllerRole, PhysicalGamepadUpdate, PhysicalInputRouter, PhysicalSourceId,
    },
};
use std::{collections::HashMap, sync::Arc};
use tokio::{sync::broadcast, task::JoinHandle};

#[derive(Clone, Debug)]
pub struct BluetoothControllerState {
    pub source_id: PhysicalSourceId,
    pub controller_id: ControllerId,
    pub session_id: SessionId,
    pub name: String,
    pub profile_id: String,
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
    pub player_index: u8,
    pub connected: bool,
    pub role: PhysicalControllerRole,
    pub state: GamepadState,
    pub battery_level: Option<u8>,
    pub charging: bool,
    pub pointer_source: Option<PointerSource>,
    pub motion: Option<MotionFrame>,
    pub report_count: u64,
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum BluetoothProviderEvent {
    Manager(ManagerEvent),
    Input(Box<BluetoothControllerState>),
    UnsupportedReportFormat {
        controller_id: ControllerId,
        profile_id: String,
    },
}

struct LiveController {
    snapshot: BluetoothControllerState,
    decoder: Option<Box<dyn ReportDecoder>>,
}

/// A running Bluetooth capture provider. The manager remains available for UI
/// discovery/pairing controls while normalized reports flow through `router`.
pub struct BluetoothInputProvider {
    manager: BluetoothManager,
    events: broadcast::Sender<BluetoothProviderEvent>,
    controllers: Arc<RwLock<HashMap<ControllerId, LiveController>>>,
    router: PhysicalInputRouter,
    task: JoinHandle<()>,
}

impl BluetoothInputProvider {
    pub async fn start(config: ManagerConfig, router: PhysicalInputRouter) -> Result<Self> {
        let manager = BluetoothManager::start(config).await?;
        let manager_events = manager.events();
        let (events, _) = broadcast::channel(1024);
        let controllers = Arc::new(RwLock::new(HashMap::new()));
        let task = tokio::spawn(run_provider(
            manager_events,
            events.clone(),
            controllers.clone(),
            router.clone(),
        ));
        Ok(Self {
            manager,
            events,
            controllers,
            router,
            task,
        })
    }

    pub fn manager(&self) -> BluetoothManager {
        self.manager.clone()
    }

    pub fn events(&self) -> broadcast::Receiver<BluetoothProviderEvent> {
        self.events.subscribe()
    }

    pub fn controllers(&self) -> Vec<BluetoothControllerState> {
        self.controllers
            .read()
            .values()
            .map(|controller| controller.snapshot.clone())
            .collect()
    }

    pub fn recenter(&self, source: &PhysicalSourceId) -> bool {
        let mut controllers = self.controllers.write();
        let Some(controller) = controllers
            .values_mut()
            .find(|controller| &controller.snapshot.source_id == source)
        else {
            return false;
        };
        controller
            .decoder
            .as_mut()
            .is_some_and(|decoder| decoder.recenter())
    }

    /// Routes emulator rumble back to every physical source composing a slot.
    pub async fn apply_feedback(&self, feedback: GamepadFeedback) -> Result<()> {
        let sources = self.router.sources_for_slot(feedback.slot);
        let sessions = {
            let controllers = self.controllers.read();
            sources
                .iter()
                .filter_map(|source| {
                    controllers
                        .values()
                        .find(|controller| {
                            controller.snapshot.connected
                                && &controller.snapshot.source_id == source
                        })
                        .map(|controller| controller.snapshot.session_id)
                })
                .collect::<Vec<_>>()
        };
        for session_id in sessions {
            self.manager
                .set_rumble(session_id, feedback.low_frequency, feedback.high_frequency)
                .await?;
        }
        Ok(())
    }

    pub async fn shutdown(self) -> Result<()> {
        self.task.abort();
        self.manager.shutdown().await
    }
}

impl Drop for BluetoothInputProvider {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn run_provider(
    mut manager_events: broadcast::Receiver<ManagerEvent>,
    events: broadcast::Sender<BluetoothProviderEvent>,
    controllers: Arc<RwLock<HashMap<ControllerId, LiveController>>>,
    router: PhysicalInputRouter,
) {
    loop {
        let event = match manager_events.recv().await {
            Ok(event) => event,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => break,
        };
        match &event {
            ManagerEvent::Connected(connected) => {
                connect_controller(connected, &events, &controllers, &router);
            }
            ManagerEvent::RawReport {
                controller_id,
                session_id,
                received_at,
                bytes,
            } => {
                let snapshot = {
                    let mut controllers = controllers.write();
                    let Some(controller) = controllers.get_mut(controller_id) else {
                        continue;
                    };
                    if controller.snapshot.session_id != *session_id {
                        continue;
                    }
                    let Some(decoded) = controller
                        .decoder
                        .as_mut()
                        .and_then(|decoder| decoder.decode(bytes, *received_at))
                    else {
                        continue;
                    };
                    controller.snapshot.connected = true;
                    controller.snapshot.role = decoded.role;
                    controller.snapshot.state = decoded.state;
                    controller.snapshot.battery_level = decoded.battery_level;
                    controller.snapshot.charging = decoded.charging;
                    controller.snapshot.pointer_source = decoded.pointer_source;
                    controller.snapshot.motion = decoded.motion;
                    controller.snapshot.report_count =
                        controller.snapshot.report_count.wrapping_add(1);
                    controller.snapshot.clone()
                };
                router.publish(PhysicalGamepadUpdate {
                    source: snapshot.source_id.clone(),
                    role: snapshot.role,
                    connected: true,
                    state: snapshot.state,
                });
                let _ = events.send(BluetoothProviderEvent::Input(Box::new(snapshot)));
            }
            ManagerEvent::Disconnected {
                controller_id,
                session_id,
                ..
            } => {
                let snapshot = {
                    let mut controllers = controllers.write();
                    let Some(controller) = controllers.get_mut(controller_id) else {
                        continue;
                    };
                    if controller.snapshot.session_id != *session_id {
                        continue;
                    }
                    controller.snapshot.connected = false;
                    controller.snapshot.state = GamepadState::default();
                    controller.snapshot.motion = None;
                    controller.snapshot.clone()
                };
                router.publish(PhysicalGamepadUpdate {
                    source: snapshot.source_id.clone(),
                    role: snapshot.role,
                    connected: false,
                    state: GamepadState::default(),
                });
                let _ = events.send(BluetoothProviderEvent::Input(Box::new(snapshot)));
            }
            _ => {}
        }
        let _ = events.send(BluetoothProviderEvent::Manager(event));
    }
}

fn connect_controller(
    connected: &ConnectedController,
    events: &broadcast::Sender<BluetoothProviderEvent>,
    controllers: &RwLock<HashMap<ControllerId, LiveController>>,
    router: &PhysicalInputRouter,
) {
    let decoder = decoder::for_profile(&connected.profile_id);
    let source_id = source_id(connected.controller_id);
    let snapshot = BluetoothControllerState {
        source_id: source_id.clone(),
        controller_id: connected.controller_id,
        session_id: connected.session_id,
        name: connected
            .identity
            .name
            .clone()
            .unwrap_or_else(|| connected.profile_id.as_str().to_owned()),
        profile_id: connected.profile_id.as_str().to_owned(),
        vendor_id: connected.identity.pnp.as_ref().map(|pnp| pnp.vendor_id),
        product_id: connected.identity.pnp.as_ref().map(|pnp| pnp.product_id),
        player_index: connected.player_index,
        connected: true,
        role: role_for_profile(connected.profile_id.as_str()),
        state: GamepadState::default(),
        battery_level: None,
        charging: false,
        pointer_source: None,
        motion: None,
        report_count: 0,
    };
    if decoder.is_none() {
        let _ = events.send(BluetoothProviderEvent::UnsupportedReportFormat {
            controller_id: connected.controller_id,
            profile_id: snapshot.profile_id.clone(),
        });
    }
    router.publish(PhysicalGamepadUpdate {
        source: source_id,
        role: snapshot.role,
        connected: true,
        state: GamepadState::default(),
    });
    controllers.write().insert(
        connected.controller_id,
        LiveController { snapshot, decoder },
    );
}

pub fn source_id(controller: ControllerId) -> PhysicalSourceId {
    let transport = match controller.transport {
        TransportKind::Ble => "ble",
        TransportKind::BrEdr => "br-edr",
    };
    PhysicalSourceId::new(format!("bt://{transport}/{}", controller.address))
}

fn role_for_profile(profile: &str) -> PhysicalControllerRole {
    if profile.ends_with("joycon-left") {
        PhysicalControllerRole::LeftHalf
    } else if profile.ends_with("joycon-right") {
        PhysicalControllerRole::RightHalf
    } else {
        PhysicalControllerRole::Whole
    }
}
