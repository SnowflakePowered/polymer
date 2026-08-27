use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use btleplug::{
    api::{
        Central, CentralEvent, Characteristic, ConnectionParameterPreset, Manager as _,
        Peripheral as _, ScanFilter, WriteType,
    },
    platform::{Adapter, Manager, Peripheral},
};
use bytes::Bytes;
use futures::StreamExt;
use tokio::{
    sync::broadcast,
    task::JoinHandle,
    time::{self, Instant},
};
use uuid::Uuid;

use crate::{ConnectionState, DeviceAddr, Error, ReportReceiver, Result, Transport};

const DISCONNECTED: u8 = 0;
const CONNECTING: u8 = 1;
const CONNECTED: u8 = 2;

#[derive(Clone, Debug)]
pub struct GattProfile {
    pub service: Uuid,
    pub input: Uuid,
    pub input_fallbacks: Vec<Uuid>,
    pub output: Uuid,
    pub response: Option<Uuid>,
    pub output_write_type: WriteType,
    /// Optional inactivity timeout for transports whose platform connection
    /// state is not updated reliably without notification traffic.
    pub idle_disconnect_timeout: Option<Duration>,
}

#[derive(Clone, Debug)]
pub struct BleDevice {
    pub addr: DeviceAddr,
    pub name: Option<String>,
    pub rssi: Option<i16>,
    pub services: Vec<Uuid>,
    pub manufacturer_data: HashMap<u16, Vec<u8>>,
}

/// Scans all advertisements visible through one OS-owned BLE adapter.
pub async fn scan(adapter_index: usize, duration: Duration) -> Result<Vec<BleDevice>> {
    let manager = Manager::new().await?;
    let adapter = manager
        .adapters()
        .await?
        .into_iter()
        .nth(adapter_index)
        .ok_or(Error::Unsupported(
            "selected OS Bluetooth adapter does not exist",
        ))?;
    adapter.start_scan(ScanFilter::default()).await?;
    time::sleep(duration).await;
    let peripherals = adapter.peripherals().await?;
    let stop_result = adapter.stop_scan().await;
    let mut found = Vec::new();
    for peripheral in peripherals {
        let Some(properties) = peripheral.properties().await? else {
            continue;
        };
        found.push(BleDevice {
            addr: DeviceAddr::public(peripheral.address().into_inner()),
            name: properties.local_name,
            rssi: properties.rssi,
            services: properties.services,
            manufacturer_data: properties.manufacturer_data,
        });
    }
    stop_result?;
    found.sort_by_key(|device| std::cmp::Reverse(device.rssi));
    Ok(found)
}

/// Stops scanning on an OS-owned adapter. This exists separately so a UI can
/// cancel a long-running scan future without leaving WinRT/BlueZ scanning in
/// the background.
pub async fn stop_scan(adapter_index: usize) -> Result<()> {
    let manager = Manager::new().await?;
    let adapter = manager
        .adapters()
        .await?
        .into_iter()
        .nth(adapter_index)
        .ok_or(Error::Unsupported(
            "selected OS Bluetooth adapter does not exist",
        ))?;
    adapter.stop_scan().await?;
    Ok(())
}

/// Returns the OS default Bluetooth adapter's public address. Windows exposes
/// this separately from GATT.
#[cfg(windows)]
pub async fn default_adapter_address() -> Result<DeviceAddr> {
    use windows::Devices::Bluetooth::BluetoothAdapter;

    let operation = BluetoothAdapter::GetDefaultAsync()
        .map_err(|error| Error::Platform(format!("could not query default adapter: {error}")))?;
    let adapter = operation
        .get()
        .map_err(|error| Error::Platform(format!("could not open default adapter: {error}")))?;
    let value = adapter
        .BluetoothAddress()
        .map_err(|error| Error::Platform(format!("could not read adapter address: {error}")))?;
    Ok(DeviceAddr::public([
        (value >> 40) as u8,
        (value >> 32) as u8,
        (value >> 24) as u8,
        (value >> 16) as u8,
        (value >> 8) as u8,
        value as u8,
    ]))
}

#[cfg(not(windows))]
pub async fn default_adapter_address() -> Result<DeviceAddr> {
    Err(Error::Unsupported(
        "automatic BLE adapter address lookup is not implemented on this platform",
    ))
}

pub struct BleGatt {
    adapter_index: usize,
    profile: GattProfile,
    reports: broadcast::Sender<Bytes>,
    responses: broadcast::Sender<Bytes>,
    disconnects: broadcast::Sender<()>,
    state: Arc<AtomicU8>,
    scan_timeout: Duration,
    peripheral: Option<Peripheral>,
    input: Option<Characteristic>,
    output: Option<Characteristic>,
    response: Option<Characteristic>,
    notification_task: Option<JoinHandle<()>>,
    failed_attempts: u8,
    next_attempt: Option<Instant>,
}

async fn subscribe_with_retry(
    peripheral: &Peripheral,
    characteristic: &Characteristic,
    label: &'static str,
) -> Result<()> {
    for attempt in 1..=3 {
        match peripheral.subscribe(characteristic).await {
            Ok(()) => return Ok(()),
            Err(error) if attempt < 3 => {
                tracing::debug!(attempt, %error, label, "retrying Switch 2 notification subscription");
                time::sleep(Duration::from_millis(250 * attempt)).await;
            }
            Err(error) => return Err(error.into()),
        }
    }
    unreachable!("notification subscription loop always returns")
}

impl BleGatt {
    pub fn new(adapter_index: usize, profile: GattProfile) -> Self {
        let (reports, _) = broadcast::channel(256);
        let (responses, _) = broadcast::channel(32);
        let (disconnects, _) = broadcast::channel(8);
        Self {
            adapter_index,
            profile,
            reports,
            responses,
            disconnects,
            state: Arc::new(AtomicU8::new(DISCONNECTED)),
            scan_timeout: Duration::from_secs(20),
            peripheral: None,
            input: None,
            output: None,
            response: None,
            notification_task: None,
            failed_attempts: 0,
            next_attempt: None,
        }
    }

    pub fn set_scan_timeout(&mut self, timeout: Duration) {
        self.scan_timeout = timeout;
    }

    pub async fn adapter_names() -> Result<Vec<String>> {
        let manager = Manager::new().await?;
        let adapters = manager.adapters().await?;
        let mut names = Vec::with_capacity(adapters.len());
        for adapter in adapters {
            names.push(adapter.adapter_info().await?);
        }
        Ok(names)
    }

    async fn adapter(&self) -> Result<Adapter> {
        Manager::new()
            .await?
            .adapters()
            .await?
            .into_iter()
            .nth(self.adapter_index)
            .ok_or(Error::Unsupported(
                "selected OS Bluetooth adapter does not exist",
            ))
    }

    async fn discover(&self, adapter: &Adapter, addr: DeviceAddr) -> Result<Peripheral> {
        // Scanning begins before the controller is expected to power on. Stop
        // immediately after finding it to return airtime to connected devices.
        // `peripherals()` includes stale WinRT cache entries. Waiting for a
        // fresh advertisement event prevents connecting through stale cache
        // state and supports devices that wake by advertising.
        let mut events = adapter.events().await?;
        adapter.start_scan(ScanFilter::default()).await?;
        let target = addr.bytes();
        let result = time::timeout(self.scan_timeout, async {
            loop {
                let event = events.next().await.ok_or(Error::ChannelClosed)?;
                let id = match event {
                    CentralEvent::DeviceDiscovered(id)
                    | CentralEvent::DeviceUpdated(id)
                    | CentralEvent::ManufacturerDataAdvertisement { id, .. }
                    | CentralEvent::RssiUpdate { id, .. } => id,
                    _ => continue,
                };
                let peripheral = adapter.peripheral(&id).await?;
                if peripheral.address().into_inner() == target {
                    return Ok::<Peripheral, Error>(peripheral);
                }
            }
        })
        .await;
        let stop_result = adapter.stop_scan().await;
        let peripheral = result.map_err(|_| Error::Timeout("BLE advertisement"))??;
        stop_result?;
        Ok(peripheral)
    }

    async fn configure(&mut self, peripheral: Peripheral) -> Result<()> {
        peripheral
            .connect_with_timeout(Duration::from_secs(15))
            .await?;
        let configured = async {
            peripheral
                .discover_services_with_timeout(Duration::from_secs(10))
                .await?;
            let characteristics = peripheral.characteristics();
            let input = characteristics
                .iter()
                .find(|c| c.service_uuid == self.profile.service && c.uuid == self.profile.input)
                .or_else(|| {
                    characteristics.iter().find(|c| {
                        c.service_uuid == self.profile.service
                            && self.profile.input_fallbacks.contains(&c.uuid)
                    })
                })
                .cloned()
                .ok_or(Error::Unsupported(
                    "BLE input notification characteristic is missing",
                ))?;
            let output = characteristics
                .iter()
                .find(|c| c.service_uuid == self.profile.service && c.uuid == self.profile.output)
                .cloned()
                .ok_or(Error::Unsupported("BLE output characteristic is missing"))?;
            let response = match self.profile.response {
                Some(uuid) => Some(
                    characteristics
                        .iter()
                        .find(|c| c.service_uuid == self.profile.service && c.uuid == uuid)
                        .cloned()
                        .ok_or(Error::Unsupported(
                            "BLE command response characteristic is missing",
                        ))?,
                ),
                None => None,
            };
            let notifications = peripheral.notifications().await?;
            if let Some(response) = &response {
                subscribe_with_retry(&peripheral, response, "command response").await?;
            }
            subscribe_with_retry(&peripheral, &input, "input").await?;

            // WinRT otherwise commonly chooses a roughly 60 ms interval,
            // throttling these high-rate notifications. Unsupported platforms
            // simply retain their default interval.
            let _ = peripheral
                .request_connection_parameters(ConnectionParameterPreset::ThroughputOptimized)
                .await;

            Ok::<_, Error>((input, output, response, notifications))
        }
        .await;
        let (input, output, response, mut notifications) = match configured {
            Ok(configured) => configured,
            Err(error) => {
                let _ = peripheral.disconnect().await;
                return Err(error);
            }
        };

        let reports = self.reports.clone();
        let responses = self.responses.clone();
        let disconnects = self.disconnects.clone();
        let state = self.state.clone();
        let input_uuid = input.uuid;
        let response_uuid = response.as_ref().map(|characteristic| characteristic.uuid);
        let idle_disconnect_timeout = self.profile.idle_disconnect_timeout;
        let monitored_peripheral = peripheral.clone();
        let notification_task = tokio::spawn(async move {
            let mut last_notification = Instant::now();
            let mut connection_check = time::interval_at(
                Instant::now() + Duration::from_millis(500),
                Duration::from_millis(500),
            );
            loop {
                tokio::select! {
                    notification = notifications.next() => {
                        let Some(notification) = notification else { break; };
                        if notification.uuid == input_uuid {
                            last_notification = Instant::now();
                            let _ = reports.send(Bytes::from(notification.value));
                        } else if Some(notification.uuid) == response_uuid {
                            let _ = responses.send(Bytes::from(notification.value));
                        }
                    }
                    _ = connection_check.tick() => {
                        if idle_disconnect_timeout
                            .is_some_and(|timeout| last_notification.elapsed() >= timeout)
                            || !monitored_peripheral.is_connected().await.unwrap_or(false)
                        {
                            break;
                        }
                    }
                }
            }
            state.store(DISCONNECTED, Ordering::Release);
            let _ = disconnects.send(());
        });
        self.peripheral = Some(peripheral);
        self.input = Some(input);
        self.output = Some(output);
        self.response = response;
        self.notification_task = Some(notification_task);
        self.state.store(CONNECTED, Ordering::Release);
        Ok(())
    }

    fn clear(&mut self) {
        if let Some(task) = self.notification_task.take() {
            task.abort();
        }
        self.input = None;
        self.output = None;
        self.response = None;
        self.peripheral = None;
        self.state.store(DISCONNECTED, Ordering::Release);
    }

    /// Notifies when the remote BLE link disappears. This remains separate
    /// from HID reports so an idle controller can be reconnected without
    /// fabricating an input packet.
    pub fn disconnections(&self) -> broadcast::Receiver<()> {
        self.disconnects.subscribe()
    }
}

#[async_trait]
impl Transport for BleGatt {
    async fn connect(&mut self, addr: DeviceAddr) -> Result<()> {
        if let Some(remaining) = self
            .next_attempt
            .and_then(|next| next.checked_duration_since(Instant::now()))
        {
            return Err(Error::Backoff(remaining));
        }
        if self.peripheral.is_some() {
            self.disconnect().await?;
        }
        self.state.store(CONNECTING, Ordering::Release);
        let result = async {
            let adapter = self.adapter().await?;
            let peripheral = self.discover(&adapter, addr).await?;
            self.configure(peripheral).await
        }
        .await;
        if result
            .as_ref()
            .is_err_and(|error| error.is_ble_device_absent())
        {
            // An absent sleeping controller is normal. Waiting for its next
            // advertisement (and WinRT stale-cache DeviceNotFound) must not
            // increase the hardware cooldown.
            self.clear();
            self.next_attempt = None;
        } else if result.is_err() {
            self.clear();
            self.failed_attempts = self.failed_attempts.saturating_add(1);
            let exponent = self.failed_attempts.saturating_sub(1).min(7);
            let seconds = 2u64.saturating_mul(1u64 << exponent).min(300);
            self.next_attempt = Some(Instant::now() + Duration::from_secs(seconds));
        } else {
            self.failed_attempts = 0;
            self.next_attempt = None;
        }
        result
    }

    fn reports(&self) -> ReportReceiver {
        self.reports.subscribe()
    }

    fn responses(&self) -> Option<ReportReceiver> {
        self.response.as_ref()?;
        Some(self.responses.subscribe())
    }

    async fn send(&mut self, data: &[u8]) -> Result<()> {
        let peripheral = self.peripheral.as_ref().ok_or(Error::ChannelClosed)?;
        let output = self.output.as_ref().ok_or(Error::ChannelClosed)?;
        peripheral
            .write(output, data, self.profile.output_write_type)
            .await?;
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        let peripheral = self.peripheral.clone();
        let input = self.input.clone();
        let response = self.response.clone();
        self.clear();
        if let Some(peripheral) = peripheral {
            if let Some(input) = input {
                let _ = peripheral.unsubscribe(&input).await;
            }
            if let Some(response) = response {
                let _ = peripheral.unsubscribe(&response).await;
            }
            if peripheral.is_connected().await? {
                peripheral.disconnect().await?;
            }
        }
        Ok(())
    }

    fn state(&self) -> ConnectionState {
        match self.state.load(Ordering::Acquire) {
            CONNECTING => ConnectionState::Connecting,
            CONNECTED => ConnectionState::Connected,
            _ => ConnectionState::Disconnected,
        }
    }
}

impl Drop for BleGatt {
    fn drop(&mut self) {
        self.clear();
    }
}
