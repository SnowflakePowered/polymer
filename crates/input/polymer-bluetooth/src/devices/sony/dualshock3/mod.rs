//! Cable-provisioned, controller-initiated Bluetooth HID lifecycle.
use crate::{
    Error, Result, Transport, TransportKind,
    devices::{
        ConnectOptions, ConnectionPolicy, ControllerDevice, ControllerSession, DeviceIdentity,
        MatchConfidence, PairingPolicy, ProfileId, ReconnectPolicy, VendorProductId,
    },
    transport::HidReportType,
};
use async_trait::async_trait;
use std::{sync::Arc, time::Duration};

#[derive(Default)]
pub struct DualShock3;

#[async_trait]
impl ControllerDevice for DualShock3 {
    fn id(&self) -> ProfileId {
        ProfileId::new("sony.dualshock3")
    }
    fn display_name(&self) -> &'static str {
        "DualShock 3"
    }
    fn matches(&self, identity: &DeviceIdentity) -> Option<MatchConfidence> {
        if identity.transport != TransportKind::BrEdr {
            return None;
        }
        if identity.has_pnp(0x054c, &[0x0268, 0x042f]) {
            return Some(MatchConfidence::Pnp);
        }
        identity
            .name_is("PLAYSTATION(R)3 Controller")
            .then_some(MatchConfidence::Model)
    }
    fn vendor_product(&self, identity: &DeviceIdentity) -> Option<VendorProductId> {
        identity
            .vendor_product()
            .or(Some(VendorProductId::new(0x054c, 0x0268)))
    }
    fn serial(&self, identity: &DeviceIdentity) -> Option<Arc<str>> {
        Some(Arc::from(identity.address.to_string()))
    }
    fn connection_policy(&self, _: &DeviceIdentity, _: &ConnectOptions) -> ConnectionPolicy {
        ConnectionPolicy {
            pairing: PairingPolicy::PseudoOob,
            reconnect: ReconnectPolicy::ControllerInitiated,
            controller_opens_hid: true,
            wait_for_encryption_before_inbound_hid: false,
            automatic_flush_slots: Some(0),
            ..ConnectionPolicy::default()
        }
    }
    fn preferred_input_report_id(&self) -> Option<u8> {
        Some(1)
    }
    fn connect(&self, options: &ConnectOptions) -> Box<dyn ControllerSession> {
        Box::new(Session {
            player: options.player_index,
            low: 0,
            high: 0,
        })
    }
}

struct Session {
    player: u8,
    low: u16,
    high: u16,
}

impl Session {
    fn output(&self) -> Result<[u8; 49]> {
        let leds = [0x02, 0x04, 0x08, 0x10, 0x12, 0x14, 0x18];
        let led = *leds
            .get(usize::from(self.player))
            .ok_or(Error::InvalidPacket("DS3 player index must be 0..6"))?;
        let mut report = [0; 49];
        report[0] = 1;
        report[2] = 0xff; // small motor duration
        report[3] = u8::from(self.high != 0);
        report[4] = 0xff; // large motor duration
        report[5] = (self.low >> 8) as u8;
        report[10] = led;
        for block in report[11..31].chunks_exact_mut(5) {
            block.copy_from_slice(&[0xff, 0x27, 0x10, 0, 0x32]);
        }
        Ok(report)
    }
}

#[async_trait]
impl ControllerSession for Session {
    async fn synchronize(&mut self, transport: &mut dyn Transport) -> Result<()> {
        let output = self.output()?;
        let mut input = transport.reports();
        // HIDP 0x53 on PSM 0x11, not DATA/Output on PSM 0x13. Same command
        // as DsHidMini DS3_BTH_SIXAXIS_ENABLE and Linux hid-sony's BT setup.
        transport
            .set_report(HidReportType::Feature, &[0xf4, 0x42, 0x03, 0, 0])
            .await?;
        transport.set_report(HidReportType::Output, &output).await?;
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                match input.recv().await {
                    Ok(report) if report.len() == 49 && report[0] == 1 => return Ok(()),
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        return Err(Error::ChannelClosed);
                    }
                }
            }
        })
        .await
        .map_err(|_| Error::Timeout("DS3 first Bluetooth input report"))??;
        tracing::info!(player_index = self.player, "DS3 Bluetooth input activated");
        Ok(())
    }
    async fn set_player_led(&mut self, transport: &mut dyn Transport, slot: u8) -> Result<()> {
        let previous = self.player;
        self.player = slot;
        let result = async {
            transport
                .set_report(HidReportType::Output, &self.output()?)
                .await
        }
        .await;
        if result.is_err() {
            self.player = previous;
        }
        result
    }
    async fn set_rumble(
        &mut self,
        transport: &mut dyn Transport,
        low: u16,
        high: u16,
    ) -> Result<()> {
        let previous = (self.low, self.high);
        (self.low, self.high) = (low, high);
        let result = async {
            transport
                .set_report(HidReportType::Output, &self.output()?)
                .await
        }
        .await;
        if result.is_err() {
            (self.low, self.high) = previous;
        }
        result
    }
}

#[cfg(test)]
mod tests;
