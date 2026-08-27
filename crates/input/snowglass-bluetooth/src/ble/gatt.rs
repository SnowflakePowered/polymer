use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

use aes::{
    Aes128,
    cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray},
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

use crate::{
    ConnectionState, DeviceAddr, Error, ReportReceiver, Result, Transport,
    ble::{
        SWITCH2_COMMAND_UUID, SWITCH2_GAMECUBE_INPUT_UUID, SWITCH2_INPUT_UUID,
        SWITCH2_JOYCON_LEFT_INPUT_UUID, SWITCH2_JOYCON_RIGHT_INPUT_UUID, SWITCH2_PRO_INPUT_UUID,
        SWITCH2_RESPONSE_UUID, SWITCH2_SERVICE_UUID, SWITCH2_SPI_INIT, switch2_feature_command,
        switch2_sleep_command,
    },
};

const DISCONNECTED: u8 = 0;
const CONNECTING: u8 = 1;
const CONNECTED: u8 = 2;

#[derive(Clone, Debug)]
pub struct GattInitWrite {
    pub characteristic: Uuid,
    pub data: Bytes,
    pub write_type: WriteType,
    pub delay_after: Duration,
}

#[derive(Clone, Debug)]
pub struct GattProfile {
    pub service: Uuid,
    pub input: Uuid,
    pub output: Uuid,
    pub response: Option<Uuid>,
    pub output_write_type: WriteType,
    /// Writes performed synchronously immediately after notification
    /// subscription. Switch 2 SPI calibration initialization belongs here.
    pub initialization: Vec<GattInitWrite>,
}

impl GattProfile {
    /// Switch 2 proprietary GATT profile. The ordering is intentional: the
    /// calibration read must follow notification subscription immediately.
    pub fn switch2() -> Self {
        Self::switch2_for(Switch2Kind::Unknown)
    }

    /// Chooses the model-specific input report where the advertisement
    /// identifies it. In particular, the NSO GameCube controller uses report
    /// 0x0A on its own notification characteristic so analog triggers remain
    /// visible in the raw viewer.
    pub fn switch2_for(kind: Switch2Kind) -> Self {
        let input = match kind {
            Switch2Kind::JoyConLeft => SWITCH2_JOYCON_LEFT_INPUT_UUID,
            Switch2Kind::JoyConRight => SWITCH2_JOYCON_RIGHT_INPUT_UUID,
            Switch2Kind::ProController => SWITCH2_PRO_INPUT_UUID,
            Switch2Kind::NsoGameCube => SWITCH2_GAMECUBE_INPUT_UUID,
            Switch2Kind::Unknown => SWITCH2_INPUT_UUID,
        };
        // Official Bluetooth initialisation uses a different feature set for
        // each controller family. Unknown devices get the common subset.
        let feature_mask = match kind {
            Switch2Kind::JoyConLeft | Switch2Kind::JoyConRight => 0x37,
            Switch2Kind::ProController => 0x2f,
            Switch2Kind::NsoGameCube | Switch2Kind::Unknown => 0x27,
        };
        Self {
            service: SWITCH2_SERVICE_UUID,
            input,
            output: SWITCH2_COMMAND_UUID,
            response: Some(SWITCH2_RESPONSE_UUID),
            output_write_type: WriteType::WithoutResponse,
            initialization: vec![
                GattInitWrite {
                    characteristic: SWITCH2_COMMAND_UUID,
                    data: Bytes::from_static(&SWITCH2_SPI_INIT),
                    write_type: WriteType::WithoutResponse,
                    delay_after: Duration::ZERO,
                },
                GattInitWrite {
                    characteristic: SWITCH2_COMMAND_UUID,
                    data: Bytes::copy_from_slice(&switch2_feature_command(0x02, feature_mask)),
                    write_type: WriteType::WithoutResponse,
                    delay_after: Duration::from_millis(500),
                },
                GattInitWrite {
                    characteristic: SWITCH2_COMMAND_UUID,
                    data: Bytes::copy_from_slice(&switch2_feature_command(0x04, feature_mask)),
                    write_type: WriteType::WithoutResponse,
                    delay_after: Duration::ZERO,
                },
            ],
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Switch2Kind {
    JoyConLeft,
    JoyConRight,
    ProController,
    NsoGameCube,
    Unknown,
}

impl Switch2Kind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::JoyConLeft => "Joy-Con 2 (L)",
            Self::JoyConRight => "Joy-Con 2 (R)",
            Self::ProController => "Nintendo Switch 2 Pro Controller",
            Self::NsoGameCube => "Nintendo GameCube Controller",
            Self::Unknown => "Nintendo Switch 2 controller",
        }
    }
}

#[derive(Clone, Debug)]
pub struct BleDevice {
    pub addr: DeviceAddr,
    pub name: Option<String>,
    pub rssi: Option<i16>,
    pub kind: Switch2Kind,
    pub manufacturer_data: HashMap<u16, Vec<u8>>,
}

impl BleDevice {
    /// Standard pairing advertisements carry an all-zero host address. Bonded
    /// reconnect/wake advertisements carry the saved host address instead.
    pub fn is_pairing_mode(&self) -> bool {
        self.manufacturer_data
            .get(&NINTENDO_MANUFACTURER_ID)
            .is_some_and(|data| data.len() >= 16 && data[10..16].iter().all(|byte| *byte == 0))
    }

    /// Host address carried by a bonded reconnect/wake advertisement.
    /// Nintendo places it on the wire in reverse byte order.
    pub fn bonded_host(&self) -> Option<DeviceAddr> {
        let data = self.manufacturer_data.get(&NINTENDO_MANUFACTURER_ID)?;
        if data.len() < 16 || data[10..16].iter().all(|byte| *byte == 0) {
            return None;
        }
        let mut bytes = [0; 6];
        bytes.copy_from_slice(&data[10..16]);
        bytes.reverse();
        Some(DeviceAddr::public(bytes))
    }

    pub fn is_bonded_to(&self, host: DeviceAddr) -> bool {
        self.bonded_host() == Some(host)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Switch2Bond {
    pub host_address: DeviceAddr,
    pub controller_address: DeviceAddr,
}

const NINTENDO_MANUFACTURER_ID: u16 = 0x0553;
const SWITCH2_ADVERTISEMENT_PREFIX: [u8; 4] = [0x01, 0x00, 0x03, 0x7e];

fn switch2_kind(data: &[u8]) -> Option<Switch2Kind> {
    if data.len() < 5 || data[..4] != SWITCH2_ADVERTISEMENT_PREFIX {
        return None;
    }
    for pair in data[4..].windows(2) {
        match u16::from_le_bytes([pair[0], pair[1]]) {
            0x2067 => return Some(Switch2Kind::JoyConLeft),
            0x2066 | 0x2068 => return Some(Switch2Kind::JoyConRight),
            0x2069 => return Some(Switch2Kind::ProController),
            0x2073 => return Some(Switch2Kind::NsoGameCube),
            _ => {}
        }
    }
    Some(Switch2Kind::Unknown)
}

/// Scans through one OS-owned BLE adapter. Start this before powering on a
/// Switch 2 controller so its short pairing advertisement is observed.
pub async fn scan_switch2(adapter_index: usize, duration: Duration) -> Result<Vec<BleDevice>> {
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
        let kind = properties
            .manufacturer_data
            .get(&NINTENDO_MANUFACTURER_ID)
            .and_then(|data| switch2_kind(data));
        let advertised_service = properties.services.contains(&SWITCH2_SERVICE_UUID);
        let nintendo_name = properties.local_name.as_ref().is_some_and(|name| {
            let name = name.to_ascii_lowercase();
            name.contains("joy-con") || name.contains("pro controller") || name.contains("gamecube")
        });
        if kind.is_none() && !advertised_service && !nintendo_name {
            continue;
        }
        found.push(BleDevice {
            addr: DeviceAddr::public(peripheral.address().into_inner()),
            name: properties.local_name,
            rssi: properties.rssi,
            kind: kind.unwrap_or(Switch2Kind::Unknown),
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
/// this separately from GATT; Nintendo stores it in the controller bond and
/// includes it in subsequent reconnect advertisements.
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

fn pairing_address_payload(host_address: DeviceAddr) -> [u8; 14] {
    let mut reversed = host_address.bytes();
    reversed.reverse();
    let mut payload = [0; 14];
    payload[0] = 0;
    payload[1] = 2;
    payload[2..8].copy_from_slice(&reversed);
    payload[8..14].copy_from_slice(&reversed);
    payload
}

fn pairing_confirmation(ltk_wire: [u8; 16], challenge_wire: [u8; 16]) -> [u8; 16] {
    let mut key = ltk_wire;
    key.reverse();
    let mut challenge = challenge_wire;
    challenge.reverse();
    let cipher = Aes128::new(GenericArray::from_slice(&key));
    let mut block = GenericArray::clone_from_slice(&challenge);
    cipher.encrypt_block(&mut block);
    let mut response = [0; 16];
    response.copy_from_slice(&block);
    response
}

pub struct BleGatt {
    adapter_index: usize,
    profile: GattProfile,
    reports: broadcast::Sender<Bytes>,
    command_responses: broadcast::Sender<Bytes>,
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
        let (command_responses, _) = broadcast::channel(32);
        let (disconnects, _) = broadcast::channel(8);
        Self {
            adapter_index,
            profile,
            reports,
            command_responses,
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
        // Switch 2 advertisements contain only flags and Nintendo manufacturer
        // data; they do not advertise the proprietary GATT service UUID.
        // `peripherals()` includes stale WinRT cache entries. Waiting for a
        // fresh advertisement event prevents us from trying GATT against a
        // sleeping controller and is what makes button-wake reconnect safe.
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
                        c.service_uuid == self.profile.service && c.uuid == SWITCH2_INPUT_UUID
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

            // Do not yield to the notification forwarding task here. Switch 2
            // controllers emit button-zero stubs unless calibration init follows
            // subscription immediately.
            for write in &self.profile.initialization {
                let characteristic = characteristics
                    .iter()
                    .find(|c| {
                        c.service_uuid == self.profile.service && c.uuid == write.characteristic
                    })
                    .ok_or(Error::Unsupported(
                        "BLE initialization characteristic is missing",
                    ))?;
                peripheral
                    .write(characteristic, &write.data, write.write_type)
                    .await?;
                if !write.delay_after.is_zero() {
                    time::sleep(write.delay_after).await;
                }
            }
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
        let command_responses = self.command_responses.clone();
        let disconnects = self.disconnects.clone();
        let state = self.state.clone();
        let input_uuid = input.uuid;
        let response_uuid = response.as_ref().map(|characteristic| characteristic.uuid);
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
                            let _ = command_responses.send(Bytes::from(notification.value));
                        }
                    }
                    _ = connection_check.tick() => {
                        if last_notification.elapsed() >= Duration::from_secs(3)
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

    async fn switch2_command(&mut self, command: u8, subcommand: u8, data: &[u8]) -> Result<Bytes> {
        let data_len = u8::try_from(data.len())
            .map_err(|_| Error::InvalidPacket("Switch 2 command payload is too large"))?;
        let peripheral = self.peripheral.as_ref().ok_or(Error::ChannelClosed)?;
        let output = self.output.as_ref().ok_or(Error::ChannelClosed)?;
        if self.response.is_none() {
            return Err(Error::Unsupported(
                "Switch 2 command response notifications are unavailable",
            ));
        }
        let mut packet = Vec::with_capacity(8 + data.len());
        packet.extend_from_slice(&[command, 0x91, 0x01, subcommand, 0x00, data_len, 0x00, 0x00]);
        packet.extend_from_slice(data);
        let mut responses = self.command_responses.subscribe();
        peripheral
            .write(output, &packet, self.profile.output_write_type)
            .await?;
        time::timeout(Duration::from_secs(3), async {
            loop {
                match responses.recv().await {
                    Ok(response)
                        if response.len() >= 8
                            && response[0] == command
                            && response[1] == 0x01
                            && response[2] == 0x01
                            && response[3] == subcommand =>
                    {
                        return Ok(Bytes::copy_from_slice(&response[8..]));
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(Error::ChannelClosed);
                    }
                }
            }
        })
        .await
        .map_err(|_| Error::Timeout("Switch 2 command response"))?
    }

    /// Performs Nintendo's proprietary pseudo-OOB bond. No standard BLE SMP
    /// pairing is involved. The controller is only asked to commit the bond
    /// after its AES challenge response has been verified.
    pub async fn bond_switch2(&mut self, host_address: DeviceAddr) -> Result<Switch2Bond> {
        if self.profile.service != SWITCH2_SERVICE_UUID {
            return Err(Error::Unsupported(
                "Nintendo bonding requires the Switch 2 GATT profile",
            ));
        }

        let address_response = self
            .switch2_command(0x15, 0x01, &pairing_address_payload(host_address))
            .await?;
        if address_response.len() < 9 || address_response[0] != 0x01 || address_response[2] != 0x01
        {
            return Err(Error::InvalidPacket(
                "Switch 2 pairing address response is malformed",
            ));
        }
        let mut controller_bytes = [0; 6];
        controller_bytes.copy_from_slice(&address_response[3..9]);
        controller_bytes.reverse();
        let controller_address = DeviceAddr::public(controller_bytes);

        let host_key_wire = rand::random::<[u8; 16]>();
        let mut key_request = [0; 17];
        key_request[1..].copy_from_slice(&host_key_wire);
        let key_response = self.switch2_command(0x15, 0x04, &key_request).await?;
        if key_response.len() < 17 || key_response[0] != 0x01 {
            return Err(Error::InvalidPacket(
                "Switch 2 pairing key response is malformed",
            ));
        }
        let mut ltk_wire = [0; 16];
        for (index, slot) in ltk_wire.iter_mut().enumerate() {
            *slot = host_key_wire[index] ^ key_response[index + 1];
        }

        let challenge_wire = rand::random::<[u8; 16]>();
        let mut challenge_request = [0; 17];
        challenge_request[1..].copy_from_slice(&challenge_wire);
        let challenge_response = self.switch2_command(0x15, 0x02, &challenge_request).await?;
        if challenge_response.len() < 17
            || challenge_response[0] != 0x01
            || challenge_response[1..17] != pairing_confirmation(ltk_wire, challenge_wire)
        {
            return Err(Error::InvalidPacket(
                "Switch 2 pairing AES confirmation failed",
            ));
        }

        let final_response = self.switch2_command(0x15, 0x03, &[0]).await?;
        if final_response.first() != Some(&0x01) {
            return Err(Error::InvalidPacket(
                "Switch 2 pairing finalization was rejected",
            ));
        }
        Ok(Switch2Bond {
            host_address,
            controller_address,
        })
    }

    /// Powers down a connected Switch 2 controller without clearing the
    /// proprietary Nintendo bond used by normal-button wake.
    pub async fn sleep_switch2(&mut self) -> Result<()> {
        if self.profile.service != SWITCH2_SERVICE_UUID {
            return Err(Error::Unsupported(
                "Nintendo sleep requires the Switch 2 GATT profile",
            ));
        }
        let peripheral = self.peripheral.as_ref().ok_or(Error::ChannelClosed)?;
        let output = self.output.as_ref().ok_or(Error::ChannelClosed)?;
        peripheral
            .write(
                output,
                &switch2_sleep_command(),
                self.profile.output_write_type,
            )
            .await?;
        Ok(())
    }

    /// Selects one player LED using Nintendo's Bluetooth bitmask command and
    /// waits for the controller acknowledgement.
    pub async fn set_switch2_player_led(&mut self, slot: usize) -> Result<()> {
        if self.profile.service != SWITCH2_SERVICE_UUID {
            return Err(Error::Unsupported(
                "Nintendo player LEDs require the Switch 2 GATT profile",
            ));
        }
        let mut data = [0; 8];
        data[0] = 1u8 << slot.min(3);
        self.switch2_command(0x09, 0x07, &data).await?;
        Ok(())
    }

    /// Changes whether the current player LED pattern flashes. Some firmware,
    /// notably the NSO GameCube controller, acknowledges disable while making
    /// its current pattern dark, so callers must opt into this separately.
    pub async fn set_switch2_led_flashing(&mut self, enabled: bool) -> Result<()> {
        if self.profile.service != SWITCH2_SERVICE_UUID {
            return Err(Error::Unsupported(
                "Nintendo player LEDs require the Switch 2 GATT profile",
            ));
        }
        self.switch2_command(0x09, 0x08, &[u8::from(enabled), 0, 0, 0])
            .await?;
        Ok(())
    }
}

#[async_trait]
impl Transport for BleGatt {
    async fn connect(&mut self, addr: DeviceAddr) -> Result<()> {
        if self.profile.service == SWITCH2_SERVICE_UUID && self.profile.initialization.is_empty() {
            return Err(Error::Unsupported(
                "Switch 2 SPI calibration initialization is mandatory",
            ));
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_switch2_advertisement_product_ids() {
        let mut data = vec![0x01, 0x00, 0x03, 0x7e, 0xaa];
        data.extend_from_slice(&0x2067u16.to_le_bytes());
        assert_eq!(switch2_kind(&data), Some(Switch2Kind::JoyConLeft));
        data[5..7].copy_from_slice(&0x2073u16.to_le_bytes());
        assert_eq!(switch2_kind(&data), Some(Switch2Kind::NsoGameCube));
    }

    #[test]
    fn rejects_other_nintendo_advertisements() {
        assert_eq!(switch2_kind(&[1, 2, 3, 4, 5]), None);
    }

    #[test]
    fn uses_controller_specific_feature_masks() {
        let cases = [
            (Switch2Kind::JoyConLeft, 0x37),
            (Switch2Kind::JoyConRight, 0x37),
            (Switch2Kind::ProController, 0x2f),
            (Switch2Kind::NsoGameCube, 0x27),
            (Switch2Kind::Unknown, 0x27),
        ];
        for (kind, expected) in cases {
            let profile = GattProfile::switch2_for(kind);
            assert_eq!(profile.initialization[1].data[8], expected);
            assert_eq!(profile.initialization[2].data[8], expected);
        }
    }

    #[test]
    fn builds_stable_one_hot_player_led_commands() {
        use crate::ble::{switch2_disable_led_flash_command, switch2_player_led_command};

        assert_eq!(switch2_player_led_command(0)[8], 0b0001);
        assert_eq!(switch2_player_led_command(1)[8], 0b0010);
        assert_eq!(switch2_player_led_command(2)[8], 0b0100);
        assert_eq!(switch2_player_led_command(3)[8], 0b1000);
        assert_eq!(switch2_player_led_command(99)[8], 0b1000);
        assert_eq!(switch2_disable_led_flash_command()[8], 0);
    }

    #[test]
    fn builds_bluetooth_sleep_command() {
        use crate::ble::switch2_sleep_command;

        assert_eq!(
            switch2_sleep_command(),
            [
                0x06, 0x91, 0x01, 0x02, 0x00, 0x0c, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ]
        );
    }

    #[test]
    fn distinguishes_pairing_and_reconnect_advertisements() {
        let mut manufacturer_data = HashMap::new();
        manufacturer_data.insert(
            NINTENDO_MANUFACTURER_ID,
            vec![1, 0, 3, 0x7e, 5, 0x66, 0x20, 0, 1, 0, 0, 0, 0, 0, 0, 0],
        );
        let mut device = BleDevice {
            addr: DeviceAddr::public([1, 2, 3, 4, 5, 6]),
            name: None,
            rssi: None,
            kind: Switch2Kind::JoyConRight,
            manufacturer_data,
        };
        assert!(device.is_pairing_mode());
        let host = DeviceAddr::public([0x60, 0xff, 0x9e, 0x87, 0xf0, 0x8f]);
        device
            .manufacturer_data
            .get_mut(&NINTENDO_MANUFACTURER_ID)
            .unwrap()[10..16]
            .copy_from_slice(&[0x8f, 0xf0, 0x87, 0x9e, 0xff, 0x60]);
        assert!(!device.is_pairing_mode());
        assert_eq!(device.bonded_host(), Some(host));
        assert!(device.is_bonded_to(host));
    }

    #[test]
    fn builds_reversed_duplicate_host_address_pairing_payload() {
        assert_eq!(
            pairing_address_payload(DeviceAddr::public([0x50, 0x3d, 0xd1, 0xc6, 0x4d, 0x71])),
            [
                0, 2, 0x71, 0x4d, 0xc6, 0xd1, 0x3d, 0x50, 0x71, 0x4d, 0xc6, 0xd1, 0x3d, 0x50,
            ]
        );
    }

    #[test]
    fn verifies_documented_pairing_confirmation_vector() {
        let host_key = [
            0x35, 0x03, 0xe9, 0x29, 0x82, 0x87, 0x71, 0x24, 0xbe, 0xa8, 0x0c, 0x66, 0x46, 0x15,
            0x83, 0x4b,
        ];
        let device_key = [
            0x5c, 0xf6, 0xee, 0x79, 0x2c, 0xdf, 0x05, 0xe1, 0xba, 0x2b, 0x63, 0x25, 0xc4, 0x1a,
            0x5f, 0x10,
        ];
        let challenge = [
            0x6f, 0xc6, 0xdf, 0x8a, 0xd8, 0xfe, 0xdf, 0x15, 0xbb, 0x8c, 0x15, 0xe9, 0x1f, 0x32,
            0x05, 0x44,
        ];
        let mut ltk = [0; 16];
        for index in 0..16 {
            ltk[index] = host_key[index] ^ device_key[index];
        }
        assert_eq!(
            pairing_confirmation(ltk, challenge),
            [
                0x13, 0x4c, 0x97, 0xf5, 0x11, 0xb9, 0xb6, 0xdd, 0x4d, 0x86, 0xfd, 0x40, 0xf5, 0x36,
                0xe9, 0xed,
            ]
        );
    }
}
