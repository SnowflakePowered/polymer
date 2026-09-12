use crate::{
    Error, Result, Transport,
    devices::{
        ConnectOptions, ConnectionPolicy, ControllerDevice, ControllerSession, DeviceIdentity,
        MatchConfidence, ProfileId, ReconnectPolicy,
    },
};
use async_trait::async_trait;
use std::{sync::Arc, time::Duration};

pub mod motion;
pub mod protocol;
use protocol::{InputReport, Output};

#[derive(Default)]
pub struct DualShock4;

#[async_trait]
impl ControllerDevice for DualShock4 {
    fn id(&self) -> ProfileId {
        ProfileId::new("sony.dualshock4")
    }
    fn display_name(&self) -> &'static str {
        "DualShock 4"
    }
    fn matches(&self, identity: &DeviceIdentity) -> Option<MatchConfidence> {
        // 0x0ba0 is Sony's USB wireless adapter, not a BR/EDR controller.
        identity
            .has_pnp(super::SONY_VENDOR_ID, &[0x05c4, 0x09cc])
            .then_some(MatchConfidence::Pnp)
    }
    fn serial(&self, identity: &DeviceIdentity) -> Option<Arc<str>> {
        Some(Arc::from(identity.address.to_string()))
    }
    fn connection_policy(&self, _: &DeviceIdentity, _: &ConnectOptions) -> ConnectionPolicy {
        ConnectionPolicy {
            reconnect: ReconnectPolicy::ControllerInitiated,
            host_opens_hid_on_reconnect: true,
            ..ConnectionPolicy::default()
        }
    }
    fn preferred_input_report_id(&self) -> Option<u8> {
        Some(0x11)
    }
    fn connect(&self, options: &ConnectOptions) -> Box<dyn ControllerSession> {
        Box::new(Session {
            calibration: None,
            output: Output {
                player: options.player_index,
                ..Default::default()
            },
        })
    }
}

struct Session {
    calibration: Option<crate::devices::SensorCalibration>,
    output: Output,
}

impl Session {
    async fn write(&mut self, transport: &mut dyn Transport, next: Output) -> Result<()> {
        transport.send(&next.report()).await?;
        self.output = next;
        Ok(())
    }
}

#[async_trait]
impl ControllerSession for Session {
    fn sensor_calibration(&self) -> Option<crate::devices::SensorCalibration> {
        self.calibration
    }
    async fn synchronize(&mut self, transport: &mut dyn Transport) -> Result<()> {
        // Subscribe before the first output: it enables enhanced Bluetooth input.
        let mut reports = transport.reports();
        let mut disconnections = transport.disconnections();
        self.write(transport, self.output.clone()).await?;
        tokio::time::timeout(Duration::from_secs(6), async {
            loop {
                let received = tokio::select! {
                    report = reports.recv() => report,
                    _ = async {
                        match disconnections.as_mut() {
                            Some(receiver) => { let _ = receiver.recv().await; }
                            None => std::future::pending::<()>().await,
                        }
                    } => return Err(Error::InvalidPacket("DualShock4 disconnected during synchronization")),
                };
                match received {
                    Ok(data) => {
                        if InputReport::parse(&data).is_some()
                        {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => {
                        return Err(Error::InvalidPacket(
                            "DualShock4 disconnected during synchronization",
                        ));
                    }
                }
            }
            Ok(())
        })
        .await
        .map_err(|_| Error::Timeout("DualShock4 enhanced input"))??;
        // Motion is usable without factory data, so a failed calibration read
        // must not undo an otherwise healthy controller connection.
        self.calibration = match transport
            .get_report(crate::transport::HidReportType::Feature, 5, 41)
            .await
        {
            Ok(data) => motion::FactoryCalibration::parse(&data),
            Err(error) => {
                tracing::warn!(%error, "DualShock4 factory calibration read failed");
                None
            }
        };
        if self.calibration.is_none() {
            tracing::warn!(
                "DualShock4 using nominal motion scales; factory calibration unavailable or invalid"
            );
        }
        Ok(())
    }
    async fn set_player_led(&mut self, transport: &mut dyn Transport, slot: u8) -> Result<()> {
        let mut next = self.output.clone();
        next.player = slot;
        self.write(transport, next).await
    }
    async fn set_rumble(
        &mut self,
        transport: &mut dyn Transport,
        low: u16,
        high: u16,
    ) -> Result<()> {
        let mut next = self.output.clone();
        next.low = low;
        next.high = high;
        self.write(transport, next).await
    }
}

#[cfg(test)]
mod tests;
