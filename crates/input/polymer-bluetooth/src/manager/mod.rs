//! High-level Bluetooth discovery and controller lifecycle manager.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use bytes::Bytes;
use kameo::{
    Actor,
    actor::{ActorRef, Spawn, WeakActorRef},
    message::{Context, Message},
    reply::{DelegatedReply, ReplySender},
};
use polymer_command::{__kameo as kameo, Admission, native_handler};
use tokio::{sync::broadcast, task::JoinHandle, time};

const ADAPTER_INITIALIZATION_ATTEMPTS: usize = 3;
const ADAPTER_INITIALIZATION_TIMEOUT: Duration = Duration::from_secs(8);
const ADAPTER_INITIALIZATION_RETRY_DELAY: Duration = Duration::from_millis(500);
const COMMAND_QUEUE_ATTEMPTS: usize = 3;
const COMMAND_QUEUE_TIMEOUT: Duration = Duration::from_millis(250);
const COMMAND_RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECTION_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(55);

use crate::{
    DeviceAddr, Error, Result, Transport, TransportKind,
    devices::{
        ConnectOptions, ControllerRegistry, ControllerSession, DeviceIdentity, PairingPolicy,
        ProfileId,
    },
    firmware::FirmwareRegistry,
    store::{ControllerLinkKeyStore, ControllerRecord, ControllerStore, MemoryControllerStore},
    transport::{
        ble::{BleDevice, BleGatt, default_adapter_address, scan as scan_ble, stop_scan},
        bredr::{
            BrEdrHid, PairingAgent, PinCode,
            hci::{AdapterSelector, Event as HciEvent, HciHost, InquiryResult},
            l2cap::Link,
            sdp,
        },
    },
};

mod actor;
mod connection;
mod discovery;
mod protocol;
mod sessions;

pub use protocol::*;

#[derive(Clone)]
struct Discovered {
    identity: DeviceIdentity,
    bredr: Option<InquiryResult>,
    ble: Option<BleDevice>,
}

struct Session {
    info: ConnectedController,
    transport: Box<dyn Transport>,
    controller: Box<dyn ControllerSession>,
    report_task: JoinHandle<()>,
    externally_owned: bool,
}

#[derive(Clone)]
struct ConnectContext {
    config: ManagerConfig,
    host: Option<Arc<HciHost>>,
    pairing: Option<Arc<PairingAgent>>,
    keys: Arc<ControllerLinkKeyStore>,
    internal: Internal,
    events: broadcast::Sender<ManagerEvent>,
}

struct PendingOperation {
    id: SessionId,
    task: JoinHandle<()>,
}

impl PendingOperation {
    async fn cancel(mut self) {
        self.task.abort();
        let _ = (&mut self.task).await;
    }
}

impl Drop for PendingOperation {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.report_task.abort();
    }
}

struct BluetoothActor {
    config: ManagerConfig,
    bredr: Option<Arc<HciHost>>,
    pairing: Option<Arc<PairingAgent>>,
    keys: Arc<ControllerLinkKeyStore>,
    internal: Internal,
    events: broadcast::Sender<ManagerEvent>,
    discovered: HashMap<DeviceAddr, Discovered>,
    active_scans: HashSet<TransportKind>,
    scan_tasks: HashMap<TransportKind, PendingOperation>,
    connect_tasks: HashMap<DeviceAddr, PendingOperation>,
    connecting_transports: HashSet<TransportKind>,
    suspended_scans: HashSet<TransportKind>,
    next_wii_liac: bool,
    sessions: HashMap<DeviceAddr, Session>,
    next_session_id: u64,
    wake_gate: Arc<tokio::sync::Mutex<()>>,
    wake_tasks: HashMap<DeviceAddr, JoinHandle<()>>,
    pairing_task: Option<JoinHandle<Result<()>>>,
    event_task: Option<JoinHandle<()>>,
    policy: Admission,
}

impl Drop for BluetoothActor {
    fn drop(&mut self) {
        self.policy.close();
        for task in self.wake_tasks.values() {
            task.abort();
        }
        if let Some(task) = &self.pairing_task {
            task.abort();
        }
        if let Some(task) = &self.event_task {
            task.abort();
        }
    }
}

#[derive(Clone)]
struct Internal(WeakActorRef<BluetoothActor>);

impl Internal {
    fn try_send(&self, command: Command) -> Result<()> {
        self.0
            .upgrade()
            .ok_or(Error::ChannelClosed)?
            .tell(command)
            .try_send()
            .map_err(|_| Error::ChannelClosed)
    }

    async fn send(&self, command: Command) -> Result<()> {
        self.try_send(command)
    }
}
