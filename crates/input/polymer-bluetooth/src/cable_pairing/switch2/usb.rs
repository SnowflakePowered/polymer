use super::CableDeviceIdentity;
use crate::devices::nintendo::switch2::packet::CommandPacket;
use crate::{
    Error, Result,
    devices::{ProfileId, nintendo::switch2::protocol::Switch2Commands},
};
use async_trait::async_trait;
use bytes::Bytes;
use nusb::transfer::{Buffer, Bulk, In, Out};
use std::{ffi::CString, time::Duration};

pub(crate) struct UsbCommands {
    input: nusb::Endpoint<Bulk, In>,
    output: nusb::Endpoint<Bulk, Out>,
    failed: bool,
}

impl UsbCommands {
    pub async fn enumerate() -> Result<Vec<CableDeviceIdentity>> {
        let mut devices = Vec::new();
        for info in nusb::list_devices().await? {
            if info.vendor_id() != 0x057e {
                continue;
            }
            let (profile, name) = match info.product_id() {
                0x2067 => ("nintendo.switch2.joycon-left", "Joy-Con 2 (L)"),
                0x2066 | 0x2068 => ("nintendo.switch2.joycon-right", "Joy-Con 2 (R)"),
                0x2069 => ("nintendo.switch2.pro", "Nintendo Switch 2 Pro Controller"),
                0x2073 => (
                    "nintendo.switch2.nso-gamecube",
                    "Nintendo GameCube Controller",
                ),
                _ => continue,
            };
            #[cfg(windows)]
            let path = CString::new(info.instance_id().to_string_lossy().as_bytes()).unwrap();
            #[cfg(not(windows))]
            let path =
                CString::new(format!("{}:{}", info.bus_id(), info.device_address())).unwrap();
            devices.push(CableDeviceIdentity {
                profile_id: ProfileId::new(profile),
                vendor_id: info.vendor_id(),
                product_id: info.product_id(),
                name: name.into(),
                serial: info.serial_number().map(str::to_owned),
                path,
                usb: Some(info),
                #[cfg(windows)]
                dshidmini: None,
            });
        }
        Ok(devices)
    }

    pub async fn open(info: &nusb::DeviceInfo) -> Result<Self> {
        let device = info.open().await?;
        let configuration = device.active_configuration()?;
        let has_commands = configuration.interfaces().any(|interface| {
            interface
                .alt_settings()
                .any(|descriptor| descriptor.interface_number() == 1 && descriptor.class() == 0xff)
        });
        if !has_commands {
            return Err(Error::Unsupported(
                "Switch 2 vendor command interface 1 is unavailable",
            ));
        }
        let interface = device.claim_interface(1).await.map_err(|error| {
            Error::Platform(format!(
                "could not open Switch 2 USB command interface 1: {error}; close other controller applications and check that interface 1 uses WinUSB"
            ))
        })?;
        Ok(Self {
            input: interface.endpoint::<Bulk, In>(0x82)?,
            output: interface.endpoint::<Bulk, Out>(0x02)?,
            failed: false,
        })
    }

    async fn exchange(&mut self, command: u8, subcommand: u8, data: &[u8]) -> Result<Bytes> {
        let packet = CommandPacket::new(command, subcommand, 0).encode(data)?;
        self.output.submit(Buffer::from(packet));
        self.output.next_complete().await.status?;
        let request = CommandPacket::new(command, subcommand, 0);
        // A full-size 64-byte reply must complete without waiting for another
        // USB packet. Memory reads may span two packets (header + echo + data).
        let response_length = if command == 0x02 && subcommand == 0x04 {
            16 + usize::from(data.first().copied().unwrap_or(0))
        } else {
            64
        };
        loop {
            self.input.submit(Buffer::new(
                response_length.next_multiple_of(self.input.max_packet_size()),
            ));
            let response = self.input.next_complete().await;
            response.status?;
            if let Some(data) = request.response(response.buffer.as_ref())? {
                return Ok(data);
            }
        }
    }
}

#[async_trait]
impl Switch2Commands for UsbCommands {
    async fn command(&mut self, command: u8, subcommand: u8, data: &[u8]) -> Result<Bytes> {
        if self.failed {
            return Err(Error::InvalidPacket(
                "reopen Switch 2 USB session after a failed command",
            ));
        }
        // A cancelled/failed exchange must never leave stale completions usable
        // by the next pairing command. Dropping the session cancels endpoints.
        self.failed = true;
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            self.exchange(command, subcommand, data),
        )
        .await
        .map_err(|_| Error::Timeout("Switch 2 USB command response"))?;
        if result.is_ok() {
            self.failed = false;
        }
        result
    }
}
