//! High-level Bluetooth discovery and controller lifecycle manager.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use actix::prelude::ContextFutureSpawner;
use actix::{
    Actor, ActorContext, ActorFutureExt, Addr, AsyncContext, AtomicResponse, Context, Handler,
    ResponseActFuture, Running, System, WeakAddr, WrapFuture, fut,
};
use bytes::Bytes;
use futures::future::{AbortHandle, Abortable};
use tokio::{sync::broadcast, task::JoinHandle, time};

const ADAPTER_INITIALIZATION_ATTEMPTS: usize = 3;
const ADAPTER_INITIALIZATION_TIMEOUT: Duration = Duration::from_secs(8);
const ADAPTER_INITIALIZATION_RETRY_DELAY: Duration = Duration::from_millis(500);
pub const COMMAND_RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);
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
mod initialization;
mod messages;
mod protocol;
mod sessions;

pub use messages::*;
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
    report_task: Option<JoinHandle<()>>,
    externally_owned: bool,
}

#[derive(Clone)]
struct ConnectContext {
    config: ManagerConfig,
    host: Option<Arc<HciHost>>,
    pairing: Option<Arc<PairingAgent>>,
    keys: Arc<ControllerLinkKeyStore>,
    events: broadcast::Sender<ManagerEvent>,
}

struct PendingOperation {
    id: SessionId,
    abort: AbortHandle,
}

impl Drop for PendingOperation {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

impl Session {
    fn stop_reports(&mut self) {
        if let Some(task) = self.report_task.take() {
            task.abort();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop_reports();
    }
}

pub struct BluetoothActor {
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
    shutdown_complete: bool,
}

impl Drop for BluetoothActor {
    fn drop(&mut self) {
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

type Internal = WeakAddr<BluetoothActor>;
