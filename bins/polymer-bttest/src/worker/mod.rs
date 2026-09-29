use std::{
    collections::HashMap,
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use crate::input::{decoder, wiimote};
use polymer_bluetooth::{
    DeviceAddr, ReportReceiver, Transport, TransportKind,
    devices::{
        ConnectOptions, ControllerDevice, ControllerRegistry, ControllerSession, DeviceIdentity,
        ProfileId,
        nintendo::switch2::{COMMON_INPUT_UUID, UnknownSwitch2Controller},
    },
    store::{
        ControllerLinkKeyStore, ControllerRecord, ControllerStore, MemoryControllerStore,
        SqliteControllerStore,
    },
    transport::{
        ble::{
            BleDevice, BleGatt, GattNotificationReceiver, default_adapter_address,
            scan as scan_ble, stop_scan as stop_ble_adapter_scan,
        },
        bredr::{
            BrEdrHid, PairingAgent, PinCode,
            hci::{AdapterSelector, Event as HciEvent, HciHost, InquiryResult},
        },
    },
};
use tokio::{
    sync::{Mutex, mpsc as tokio_mpsc},
    task::JoinHandle,
};

mod control;
mod identity;
mod persistence;
mod run;
mod state;
mod tasks;

use control::*;
use persistence::*;
use run::*;
use state::*;
use tasks::*;

fn connect_options(slot: usize) -> ConnectOptions {
    ConnectOptions {
        player_index: u8::try_from(slot).unwrap_or(u8::MAX),
        ..ConnectOptions::default()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BondingMode {
    Ssp,
    WiiOneTwo,
    WiiSync,
}

impl BondingMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ssp => "SSP / automatic",
            Self::WiiOneTwo => "Wii Remote 1+2",
            Self::WiiSync => "Wii Remote red SYNC",
        }
    }
    pub fn help(self) -> &'static str {
        match self {
            Self::Ssp => {
                "DS4, DualSense, Switch 1 controllers, and other SSP devices. Nintendo uses 0000 if it requests a legacy PIN."
            }
            Self::WiiOneTwo => {
                "Temporary Wii sync: PIN is the Wii Remote address in reverse byte order."
            }
            Self::WiiSync => {
                "Persistent Wii sync: PIN is the dedicated adapter address in reverse byte order."
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct DeviceView {
    pub inquiry: InquiryResult,
    pub addr: String,
    pub name: String,
    pub rssi: Option<i8>,
    pub class: String,
    pub profile_id: Option<ProfileId>,
    pub profile_name: Option<&'static str>,
}

impl DeviceView {
    pub fn is_wii_remote(&self) -> bool {
        self.profile_id
            .as_ref()
            .is_some_and(|id| id.as_str().starts_with("nintendo.wiimote."))
    }

    pub fn is_switch(&self) -> bool {
        self.profile_id
            .as_ref()
            .is_some_and(|id| id.as_str().starts_with("nintendo.switch."))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadioKind {
    BrEdr,
    Ble,
}

pub enum Command {
    CablePairing(crate::cable_pairing::Action),
    BeginControllerOrder {
        adapter_index: usize,
    },
    EndControllerOrder,
    ConnectBr {
        device: InquiryResult,
        pin: BondingMode,
        profile_id: Option<ProfileId>,
    },
    ConnectBle {
        adapter_index: usize,
        device: BleDevice,
    },
    SelectSwitch2InputReport {
        addr: DeviceAddr,
        report_id: u8,
    },
    SubscribeAllSwitch2InputReports {
        addr: DeviceAddr,
    },
    Forget(DeviceAddr),
    ListenInbound(bool),
    SendOutput {
        addr: DeviceAddr,
        bytes: Vec<u8>,
    },
    SleepBle(DeviceAddr),
    Disconnect(Option<DeviceAddr>),
    Shutdown,
}

pub enum Event {
    CablePairing(crate::cable_pairing::Event),
    BrReady(String),
    BleReady(Vec<String>),
    Scanning {
        radio: RadioKind,
        enabled: bool,
        message: String,
    },
    Busy(String),
    Idle(String),
    Device(DeviceView),
    BleDevice(BleDevice),
    Connected {
        addr: String,
        name: String,
        radio: RadioKind,
        slot: usize,
    },
    Disconnected {
        addr: String,
        reason: String,
    },
    Report {
        addr: String,
        bytes: Vec<u8>,
    },
    BleReport {
        addr: String,
        characteristic: String,
        bytes: Vec<u8>,
        received_at: Instant,
    },
    BleReportsDropped {
        addr: String,
        count: u64,
    },
    Switch2InputReportSelected {
        addr: String,
        report_id: u8,
    },
    Switch2InputReportSelectionFailed {
        addr: String,
        report_id: u8,
        reason: String,
    },
    Switch2InputReportsSubscribed {
        addr: String,
        report_ids: Vec<u8>,
    },
    Switch2InputReportsSubscriptionFailed {
        addr: String,
        reason: String,
    },
    WiiTelemetry {
        addr: String,
        telemetry: wiimote::Telemetry,
        received_at: Instant,
    },
    DecodedInput {
        addr: String,
        input: decoder::DecodedInput,
    },
    Log(String),
    Error(String),
}

pub fn spawn() -> (tokio_mpsc::UnboundedSender<Command>, mpsc::Receiver<Event>) {
    reset_diagnostic_log();
    diagnostic("Starting Polymer Bluetooth test worker");
    let (command_tx, command_rx) = tokio_mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::channel();
    thread::Builder::new()
        .name("polymer-bttest".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .worker_threads(2)
                .build();
            match runtime {
                Ok(runtime) => runtime.block_on(run(command_rx, event_tx)),
                Err(error) => {
                    let _ = event_tx.send(Event::Error(format!(
                        "could not start async runtime: {error}"
                    )));
                }
            }
        })
        .expect("failed to spawn Bluetooth test worker");
    (command_tx, event_rx)
}
