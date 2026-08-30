//! Wii Remote connection-time initialization.

use std::{marker::PhantomData, time::Duration};

use async_trait::async_trait;
use tokio::sync::broadcast;

use crate::devices::{
    BondingMode, ConnectOptions, ConnectionPolicy, ControllerDevice, ControllerSession,
    DeviceIdentity, HostIdentity, IrCameraSetup, IrMode, IrSensitivity, MatchConfidence,
    PairingPolicy, ProfileId, ReconnectPolicy,
};
use crate::{Error, ReportReceiver, Result, Transport};

const LED_REPORT_ID: u8 = 0x11;
const REPORT_MODE_ID: u8 = 0x12;
const IR_ENABLE_ID: u8 = 0x13;
const IR_ENABLE_2_ID: u8 = 0x1a;
const WRITE_DATA_ID: u8 = 0x16;
const CONTINUOUS: u8 = 0x04;
const REPORT_CORE_ACCEL_IR10_EXT6: u8 = 0x37;
const I2C_CAMERA: u8 = 0x58;
const I2C_MOTION_PLUS_INACTIVE: u8 = 0x53;
const MOTION_PLUS_INIT_ADDRESS: u16 = 0x00f0;
const MOTION_PLUS_MODE_ADDRESS: u16 = 0x00fe;
const MOTION_PLUS_DISABLED_PASSTHROUGH: u8 = 0x04;
const REQUEST_ACKNOWLEDGEMENT: u8 = 0x02;
const ENABLE_FEATURE: u8 = 0x04;
const INPUT_ACKNOWLEDGEMENT_ID: u8 = 0x22;
const CAMERA_ENABLE_ADDRESS: u16 = 0x0030;
const CAMERA_SENSITIVITY_BLOCK_1_ADDRESS: u16 = 0x0000;
const CAMERA_SENSITIVITY_BLOCK_2_ADDRESS: u16 = 0x001a;
const CAMERA_MODE_ADDRESS: u16 = 0x0033;

impl IrCameraSetup {
    fn sensitivity_blocks(self) -> (&'static [u8; 9], &'static [u8; 2]) {
        const LOW: [u8; 9] = [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x90, 0x00, 0x41];
        const LOW_2: [u8; 2] = [0x40, 0x00];
        const MEDIUM: [u8; 9] = [0x02, 0x00, 0x00, 0x71, 0x01, 0x00, 0xaa, 0x00, 0x64];
        const MEDIUM_2: [u8; 2] = [0x63, 0x03];
        const HIGH: [u8; 9] = [0x07, 0x00, 0x00, 0x71, 0x01, 0x00, 0x72, 0x00, 0x20];
        const HIGH_2: [u8; 2] = [0xb3, 0x04];
        match self.sensitivity {
            IrSensitivity::Level1 | IrSensitivity::Level2 => (&LOW, &LOW_2),
            IrSensitivity::Level3 | IrSensitivity::Level4 => (&MEDIUM, &MEDIUM_2),
            IrSensitivity::Level5 => (&HIGH, &HIGH_2),
        }
    }

    fn mode_byte(self) -> u8 {
        match self.mode {
            IrMode::Basic => 0x01,
            IrMode::Extended => 0x03,
            IrMode::Full => 0x05,
        }
    }
}

#[async_trait]
trait WiimoteModel: Send + Sync + 'static {
    const PROFILE_ID: &'static str;
    const DISPLAY_NAME: &'static str;

    fn matches_wiimote(identity: &DeviceIdentity) -> Option<MatchConfidence>;

    fn policy(options: &ConnectOptions) -> ConnectionPolicy {
        let pairing = match options.bonding_mode.as_deref() {
            Some("one-two") => PairingPolicy::RemoteAddressPin,
            _ => PairingPolicy::LocalAddressPin,
        };
        ConnectionPolicy {
            pairing,
            reconnect: ReconnectPolicy::ControllerInitiated,
            // Fresh Wii Remote pairing follows the host-initiated HID path.
            // Button-wake reconnects are controller-initiated, but those enter
            // through Manager::accept_inbound rather than this outbound policy.
            controller_opens_hid: false,
            wait_for_encryption_before_inbound_hid: false,
            become_master: true,
            automatic_flush_slots: Some(0),
            host_identity: HostIdentity::SNOWGLASS,
        }
    }

    async fn synchronize(
        transport: &mut dyn Transport,
        player_index: u8,
        setup: IrCameraSetup,
    ) -> Result<()> {
        let mut reports = transport.reports();
        Self::set_led(transport, player_index, false).await?;
        Self::write_i2c(
            transport,
            &mut reports,
            I2C_MOTION_PLUS_INACTIVE,
            MOTION_PLUS_INIT_ADDRESS,
            &[0x55],
        )
        .await?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        Self::write_i2c(
            transport,
            &mut reports,
            I2C_MOTION_PLUS_INACTIVE,
            MOTION_PLUS_MODE_ADDRESS,
            &[MOTION_PLUS_DISABLED_PASSTHROUGH],
        )
        .await?;
        tokio::time::sleep(Duration::from_millis(500)).await;
        Self::initialize_ir_camera(transport, &mut reports, setup).await?;
        transport
            .send(&[REPORT_MODE_ID, CONTINUOUS, REPORT_CORE_ACCEL_IR10_EXT6])
            .await
    }

    async fn initialize_ir_camera(
        transport: &mut dyn Transport,
        reports: &mut ReportReceiver,
        setup: IrCameraSetup,
    ) -> Result<()> {
        let (block_1, block_2) = setup.sensitivity_blocks();
        Self::send_acknowledged(
            transport,
            reports,
            &[IR_ENABLE_ID, ENABLE_FEATURE | REQUEST_ACKNOWLEDGEMENT],
        )
        .await?;
        Self::send_acknowledged(
            transport,
            reports,
            &[IR_ENABLE_2_ID, ENABLE_FEATURE | REQUEST_ACKNOWLEDGEMENT],
        )
        .await?;
        Self::write_i2c(
            transport,
            reports,
            I2C_CAMERA,
            CAMERA_ENABLE_ADDRESS,
            &[0x01],
        )
        .await?;
        Self::write_i2c(
            transport,
            reports,
            I2C_CAMERA,
            CAMERA_SENSITIVITY_BLOCK_1_ADDRESS,
            block_1,
        )
        .await?;
        Self::write_i2c(
            transport,
            reports,
            I2C_CAMERA,
            CAMERA_SENSITIVITY_BLOCK_2_ADDRESS,
            block_2,
        )
        .await?;
        Self::write_i2c(
            transport,
            reports,
            I2C_CAMERA,
            CAMERA_MODE_ADDRESS,
            &[setup.mode_byte()],
        )
        .await?;
        Self::write_i2c(
            transport,
            reports,
            I2C_CAMERA,
            CAMERA_ENABLE_ADDRESS,
            &[0x08],
        )
        .await
    }

    async fn set_led(transport: &mut dyn Transport, slot: u8, rumble: bool) -> Result<()> {
        let led = slot.saturating_add(1).min(0x0f) << 4;
        transport
            .send(&[LED_REPORT_ID, led | u8::from(rumble)])
            .await
    }

    async fn write_i2c(
        transport: &mut dyn Transport,
        reports: &mut ReportReceiver,
        slave: u8,
        address: u16,
        data: &[u8],
    ) -> Result<()> {
        debug_assert!(data.len() <= 16);
        let mut report = [0u8; 22];
        report[0] = WRITE_DATA_ID;
        report[1] = ENABLE_FEATURE | REQUEST_ACKNOWLEDGEMENT;
        report[2] = slave << 1;
        report[3..5].copy_from_slice(&address.to_be_bytes());
        report[5] = data.len() as u8;
        report[6..6 + data.len()].copy_from_slice(data);
        Self::send_acknowledged(transport, reports, &report).await
    }

    async fn send_acknowledged(
        transport: &mut dyn Transport,
        reports: &mut ReportReceiver,
        report: &[u8],
    ) -> Result<()> {
        let output_report_id = report[0];
        transport.send(report).await?;
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                match reports.recv().await {
                    Ok(response)
                        if response.first() == Some(&INPUT_ACKNOWLEDGEMENT_ID)
                            && response.len() >= 5
                            && response[3] == output_report_id =>
                    {
                        return if response[4] == 0 {
                            Ok(())
                        } else {
                            Err(Error::L2cap(format!(
                                "Wii Remote rejected output report 0x{output_report_id:02x} with error 0x{:02x}",
                                response[4]
                            )))
                        };
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return Err(Error::ChannelClosed),
                }
            }
        })
        .await
        .map_err(|_| Error::Timeout("Wii Remote output report acknowledgement"))?
    }
}

struct WiimoteSession<M> {
    player_index: u8,
    rumble: bool,
    ir_camera: IrCameraSetup,
    model: PhantomData<M>,
}

#[async_trait]
impl<M: WiimoteModel> ControllerSession for WiimoteSession<M> {
    async fn synchronize(&mut self, transport: &mut dyn Transport) -> Result<()> {
        self.rumble = false;
        M::synchronize(transport, self.player_index, self.ir_camera).await
    }

    async fn set_external_ownership(
        &mut self,
        transport: &mut dyn Transport,
        enabled: bool,
    ) -> Result<()> {
        let _ = transport
            .set_automatic_flush_slots(if enabled { 96 } else { 0 })
            .await;
        if enabled {
            Ok(())
        } else {
            self.synchronize(transport).await
        }
    }

    async fn set_player_led(&mut self, transport: &mut dyn Transport, slot: u8) -> Result<()> {
        M::set_led(transport, slot, self.rumble).await?;
        self.player_index = slot;
        Ok(())
    }

    async fn set_rumble(
        &mut self,
        transport: &mut dyn Transport,
        low_frequency: u16,
        high_frequency: u16,
    ) -> Result<()> {
        let enabled = low_frequency != 0 || high_frequency != 0;
        M::set_led(transport, self.player_index, enabled).await?;
        self.rumble = enabled;
        Ok(())
    }

    async fn set_ir_camera(
        &mut self,
        transport: &mut dyn Transport,
        setup: IrCameraSetup,
    ) -> Result<()> {
        let mut reports = transport.reports();
        M::initialize_ir_camera(transport, &mut reports, setup).await?;
        transport
            .send(&[REPORT_MODE_ID, CONTINUOUS, REPORT_CORE_ACCEL_IR10_EXT6])
            .await?;
        self.ir_camera = setup;
        Ok(())
    }
}

#[doc(hidden)]
pub struct WiimoteDevice<M>(PhantomData<M>);

impl<M> Default for WiimoteDevice<M> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

#[async_trait]
impl<M> ControllerDevice for WiimoteDevice<M>
where
    M: WiimoteModel,
{
    fn id(&self) -> ProfileId {
        ProfileId::new(M::PROFILE_ID)
    }

    fn display_name(&self) -> &'static str {
        M::DISPLAY_NAME
    }

    fn matches(&self, identity: &DeviceIdentity) -> Option<MatchConfidence> {
        M::matches_wiimote(identity)
    }

    fn bonding_modes(&self) -> Vec<BondingMode> {
        vec![
            BondingMode::new("sync", "Red SYNC button"),
            BondingMode::new("one-two", "1 + 2 buttons"),
        ]
    }

    fn connection_policy(
        &self,
        _identity: &DeviceIdentity,
        options: &ConnectOptions,
    ) -> ConnectionPolicy {
        M::policy(options)
    }

    fn connect(&self, options: &ConnectOptions) -> Box<dyn ControllerSession> {
        Box::new(WiimoteSession::<M> {
            player_index: options.player_index,
            rumble: false,
            ir_camera: IrCameraSetup::default(),
            model: PhantomData,
        })
    }
}

controller_model! {
    WiiRemote = WiimoteDevice<WiiRemoteModel> impl WiimoteModel {
        const PROFILE_ID: &'static str = "nintendo.wiimote.original";
        const DISPLAY_NAME: &'static str = "Wii Remote";

        fn matches_wiimote(identity: &DeviceIdentity) -> Option<MatchConfidence> {
            identity
                .name_is("Nintendo RVL-CNT-01")
                .then_some(MatchConfidence::Model)
        }
    }
}

controller_model! {
    WiiRemotePlus = WiimoteDevice<WiiRemotePlusModel> impl WiimoteModel {
        const PROFILE_ID: &'static str = "nintendo.wiimote.plus";
        const DISPLAY_NAME: &'static str = "Wii Remote Plus";

        fn matches_wiimote(identity: &DeviceIdentity) -> Option<MatchConfidence> {
            identity
                .name_is("Nintendo RVL-CNT-01-TR")
                .then_some(MatchConfidence::Model)
        }
    }
}

controller_model! {
    UnknownWiiRemote = WiimoteDevice<UnknownWiiRemoteModel> impl WiimoteModel {
        const PROFILE_ID: &'static str = "nintendo.wiimote.unknown";
        const DISPLAY_NAME: &'static str = "Unknown Wii Remote";

        fn matches_wiimote(identity: &DeviceIdentity) -> Option<MatchConfidence> {
            identity
                .name_starts_with("Nintendo RVL-CNT")
                .then_some(MatchConfidence::Family)
        }
    }
}
