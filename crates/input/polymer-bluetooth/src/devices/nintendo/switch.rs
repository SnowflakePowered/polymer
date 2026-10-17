//! Nintendo Switch BR/EDR connection-time protocol.

use std::{marker::PhantomData, sync::Arc, time::Duration};

use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::broadcast;

use crate::devices::{
    ConnectOptions, ConnectionPolicy, ControllerDevice, ControllerSession, DeviceIdentity,
    HostIdentity, MatchConfidence, PairingPolicy, ProfileId, ReconnectPolicy, VendorProductId,
};
use crate::{Error, ReportReceiver, Result, Transport};

const OUTPUT_RUMBLE_AND_SUBCOMMAND: u8 = 0x01;
const INPUT_SUBCOMMAND_REPLY: u8 = 0x21;
const SUBCOMMAND_SET_REPORT_MODE: u8 = 0x03;
const SUBCOMMAND_SET_HCI_STATE: u8 = 0x06;
const SUBCOMMAND_SPI_FLASH_READ: u8 = 0x10;
const SUBCOMMAND_SET_PLAYER_LIGHTS: u8 = 0x30;
const SUBCOMMAND_ENABLE_IMU: u8 = 0x40;
const SUBCOMMAND_SET_IMU_SENSITIVITY: u8 = 0x41;
const SUBCOMMAND_ENABLE_VIBRATION: u8 = 0x48;
const FULL_REPORT_MODE: u8 = 0x30;
const NEUTRAL_RUMBLE: [u8; 4] = [0x00, 0x01, 0x40, 0x40];
const SERIAL_ADDRESS: u32 = 0x6000;
const SERIAL_LENGTH: usize = 16;

#[async_trait]
trait SwitchModel: Send + Sync + 'static {
    const PROFILE_ID: &'static str;
    const DISPLAY_NAME: &'static str;
    const NAMES: &'static [&'static str];
    const PRODUCT_ID: u16;
    const HAS_IMU: bool = false;

    fn product_id(_identity: &DeviceIdentity) -> u16 {
        Self::PRODUCT_ID
    }

    fn policy() -> ConnectionPolicy {
        ConnectionPolicy {
            pairing: PairingPolicy::FixedPin(b"0000"),
            reconnect: ReconnectPolicy::ControllerInitiated,
            controller_opens_hid: false,
            wait_for_encryption_before_inbound_hid: true,
            host_opens_hid_on_reconnect: false,
            become_master: false,
            automatic_flush_slots: None,
            host_identity: HostIdentity::NINTENDO,
        }
    }

    fn matches_switch(identity: &DeviceIdentity) -> Option<MatchConfidence> {
        Self::NAMES
            .iter()
            .any(|name| identity.name_is(name))
            .then_some(MatchConfidence::Model)
    }

    async fn send_subcommand(
        transport: &mut dyn Transport,
        packet_counter: &mut u8,
        subcommand: u8,
        data: &[u8],
    ) -> Result<Bytes> {
        let mut reports = transport.reports();
        for attempt in 0..3 {
            let report = Self::subcommand_report(*packet_counter, subcommand, data);
            *packet_counter = (*packet_counter + 1) & 0x0f;
            transport.send(&report).await?;
            match Self::wait_for_subcommand_reply(&mut reports, subcommand).await {
                Ok(response) => return Ok(response),
                Err(error) if attempt != 2 => {
                    tracing::debug!(subcommand, attempt, %error, "retrying Switch subcommand")
                }
                Err(error) => return Err(error),
            }
        }
        unreachable!()
    }

    fn subcommand_report(packet_counter: u8, subcommand: u8, data: &[u8]) -> [u8; 49] {
        debug_assert!(data.len() <= 38);
        let mut report = [0u8; 49];
        report[0] = OUTPUT_RUMBLE_AND_SUBCOMMAND;
        report[1] = packet_counter & 0x0f;
        report[2..6].copy_from_slice(&NEUTRAL_RUMBLE);
        report[6..10].copy_from_slice(&NEUTRAL_RUMBLE);
        report[10] = subcommand;
        report[11..11 + data.len()].copy_from_slice(data);
        report
    }

    async fn wait_for_subcommand_reply(
        reports: &mut ReportReceiver,
        subcommand: u8,
    ) -> Result<Bytes> {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                match reports.recv().await {
                    Ok(report)
                        if report.first() == Some(&INPUT_SUBCOMMAND_REPLY)
                            && report.len() >= 15
                            && report[14] == subcommand =>
                    {
                        return if report[13] & 0x80 != 0 {
                            Ok(report.slice(15..))
                        } else {
                            Err(Error::L2cap(format!(
                                "Switch subcommand 0x{subcommand:02x} was rejected"
                            )))
                        };
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return Err(Error::ChannelClosed),
                }
            }
        })
        .await
        .map_err(|_| Error::Timeout("Switch subcommand reply"))?
    }

    async fn synchronize(
        transport: &mut dyn Transport,
        player_index: u8,
        packet_counter: &mut u8,
    ) -> Result<Option<Arc<str>>> {
        *packet_counter = 0;
        if Self::HAS_IMU {
            Self::send_subcommand(transport, packet_counter, SUBCOMMAND_ENABLE_IMU, &[1]).await?;
            // Match the decoder's ±2000 dps / ±8 g scales. Configure after
            // enabling, since powering up the IMU resets its sensitivity.
            Self::send_subcommand(
                transport,
                packet_counter,
                SUBCOMMAND_SET_IMU_SENSITIVITY,
                &[3, 0, 1, 1],
            )
            .await?;
        }
        Self::send_subcommand(
            transport,
            packet_counter,
            SUBCOMMAND_SET_REPORT_MODE,
            &[FULL_REPORT_MODE],
        )
        .await?;
        let serial = match Self::read_serial(transport, packet_counter).await {
            Ok(serial) => serial,
            Err(error) => {
                tracing::warn!(
                    controller = Self::DISPLAY_NAME,
                    %error,
                    "failed to read Switch controller serial"
                );
                None
            }
        };
        Self::send_subcommand(
            transport,
            packet_counter,
            SUBCOMMAND_ENABLE_VIBRATION,
            &[0x01],
        )
        .await?;
        Self::set_led(transport, player_index, packet_counter).await?;
        tracing::debug!(
            controller = Self::DISPLAY_NAME,
            "synchronized Switch controller"
        );
        Ok(serial)
    }

    async fn read_serial(
        transport: &mut dyn Transport,
        packet_counter: &mut u8,
    ) -> Result<Option<Arc<str>>> {
        let mut request = [0; 5];
        request[..4].copy_from_slice(&SERIAL_ADDRESS.to_le_bytes());
        request[4] = SERIAL_LENGTH as u8;
        let response = Self::send_subcommand(
            transport,
            packet_counter,
            SUBCOMMAND_SPI_FLASH_READ,
            &request,
        )
        .await?;
        if response.len() < 5 + SERIAL_LENGTH
            || response[..4] != request[..4]
            || usize::from(response[4]) != SERIAL_LENGTH
        {
            return Err(Error::InvalidPacket(
                "Switch serial-number SPI response is malformed",
            ));
        }
        let bytes = &response[5..5 + SERIAL_LENGTH];
        if bytes[0] >= 0x80 {
            return Ok(None);
        }
        let bytes = &bytes[..SERIAL_LENGTH - 1];
        if bytes.iter().any(|byte| *byte >= 0x80) {
            return Err(Error::InvalidPacket("Switch serial number is not ASCII"));
        }
        let serial = bytes
            .iter()
            .filter(|byte| **byte != 0)
            .map(|byte| char::from(*byte))
            .collect::<String>();
        Ok((!serial.is_empty()).then(|| Arc::from(serial)))
    }

    async fn set_led(transport: &mut dyn Transport, slot: u8, counter: &mut u8) -> Result<()> {
        Self::send_subcommand(
            transport,
            counter,
            SUBCOMMAND_SET_PLAYER_LIGHTS,
            &[slot.saturating_add(1).min(0x0f)],
        )
        .await
        .map(|_| ())
    }
}

struct SwitchSession<M> {
    packet_counter: u8,
    player_index: u8,
    serial: Option<Arc<str>>,
    model: PhantomData<M>,
}

#[async_trait]
impl<M: SwitchModel> ControllerSession for SwitchSession<M> {
    async fn synchronize(&mut self, transport: &mut dyn Transport) -> Result<()> {
        self.serial =
            M::synchronize(transport, self.player_index, &mut self.packet_counter).await?;
        Ok(())
    }

    fn serial(&self) -> Option<Arc<str>> {
        self.serial.clone()
    }

    async fn set_player_led(&mut self, transport: &mut dyn Transport, slot: u8) -> Result<()> {
        M::set_led(transport, slot, &mut self.packet_counter).await
    }

    async fn sleep(&mut self, transport: &mut dyn Transport) -> Result<()> {
        tokio::time::sleep(Duration::from_millis(1_200)).await;
        let report = M::subcommand_report(self.packet_counter, SUBCOMMAND_SET_HCI_STATE, &[0x00]);
        self.packet_counter = (self.packet_counter + 1) & 0x0f;
        transport.send(&report).await?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        Ok(())
    }
}

#[doc(hidden)]
pub struct SwitchDevice<M>(PhantomData<M>);

impl<M> Default for SwitchDevice<M> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

#[async_trait]
impl<M> ControllerDevice for SwitchDevice<M>
where
    M: SwitchModel,
{
    fn id(&self) -> ProfileId {
        ProfileId::new(M::PROFILE_ID)
    }

    fn display_name(&self) -> &'static str {
        M::DISPLAY_NAME
    }

    fn matches(&self, identity: &DeviceIdentity) -> Option<MatchConfidence> {
        M::matches_switch(identity)
    }

    fn vendor_product(&self, identity: &DeviceIdentity) -> Option<VendorProductId> {
        identity.vendor_product().or_else(|| {
            Some(VendorProductId::new(
                super::NINTENDO_VENDOR_ID,
                M::product_id(identity),
            ))
        })
    }

    fn connection_policy(
        &self,
        _identity: &DeviceIdentity,
        _options: &ConnectOptions,
    ) -> ConnectionPolicy {
        M::policy()
    }

    fn connect(&self, options: &ConnectOptions) -> Box<dyn ControllerSession> {
        Box::new(SwitchSession::<M> {
            packet_counter: 0,
            player_index: options.player_index,
            serial: None,
            model: PhantomData,
        })
    }
}

controller_model! {
    SwitchProController = SwitchDevice<SwitchProModel> impl SwitchModel {
        const PROFILE_ID: &'static str = "nintendo.switch.pro";
        const HAS_IMU: bool = true;
        const DISPLAY_NAME: &'static str = "Nintendo Switch Pro Controller";
        const NAMES: &'static [&'static str] = &["Pro Controller"];
        const PRODUCT_ID: u16 = 0x2009;
    }
}

controller_model! {
    JoyConLeft = SwitchDevice<JoyConLeftModel> impl SwitchModel {
        const PROFILE_ID: &'static str = "nintendo.switch.joycon-left";
        const HAS_IMU: bool = true;
        const DISPLAY_NAME: &'static str = "Joy-Con (L)";
        const NAMES: &'static [&'static str] = &["Joy-Con (L)"];
        const PRODUCT_ID: u16 = 0x2006;
    }
}

controller_model! {
    JoyConRight = SwitchDevice<JoyConRightModel> impl SwitchModel {
        const PROFILE_ID: &'static str = "nintendo.switch.joycon-right";
        const HAS_IMU: bool = true;
        const DISPLAY_NAME: &'static str = "Joy-Con (R)";
        const NAMES: &'static [&'static str] = &["Joy-Con (R)"];
        const PRODUCT_ID: u16 = 0x2007;
    }
}

controller_model! {
    NsoNesController = SwitchDevice<NsoNesModel> impl SwitchModel {
        const PROFILE_ID: &'static str = "nintendo.switch.nso-nes";
        const DISPLAY_NAME: &'static str = "Nintendo Switch Online NES Controller";
        const NAMES: &'static [&'static str] =
            &["NES Controller", "HVC Controller (L)", "HVC Controller (R)"];
        const PRODUCT_ID: u16 = 0x2007;

        fn product_id(identity: &DeviceIdentity) -> u16 {
            if identity.name_is("HVC Controller (L)") {
                0x2006
            } else {
                Self::PRODUCT_ID
            }
        }
    }
}

controller_model! {
    NsoSnesController = SwitchDevice<NsoSnesModel> impl SwitchModel {
        const PROFILE_ID: &'static str = "nintendo.switch.nso-snes";
        const DISPLAY_NAME: &'static str = "Nintendo Switch Online SNES Controller";
        const NAMES: &'static [&'static str] = &["SNES Controller"];
        const PRODUCT_ID: u16 = 0x2017;
    }
}

controller_model! {
    NsoN64Controller = SwitchDevice<NsoN64Model> impl SwitchModel {
        const PROFILE_ID: &'static str = "nintendo.switch.nso-n64";
        const DISPLAY_NAME: &'static str = "Nintendo Switch Online N64 Controller";
        const NAMES: &'static [&'static str] = &["Nintendo N64 Controller", "N64 Controller"];
        const PRODUCT_ID: u16 = 0x2019;
    }
}

controller_model! {
    NsoGenesisController = SwitchDevice<NsoGenesisModel> impl SwitchModel {
        const PROFILE_ID: &'static str = "nintendo.switch.nso-genesis";
        const DISPLAY_NAME: &'static str =
            "Nintendo Switch Online Genesis / Mega Drive Controller";
        const NAMES: &'static [&'static str] = &["MD/Gen Control Pad"];
        const PRODUCT_ID: u16 = 0x201e;
    }
}
