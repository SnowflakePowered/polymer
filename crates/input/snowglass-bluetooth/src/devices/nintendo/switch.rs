//! Nintendo Switch BR/EDR connection-time protocol.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::broadcast;

use crate::devices::{ConnectOptions, ControllerDevice, binary_player_leds};
use crate::{Error, ReportReceiver, Result, Transport};

const OUTPUT_RUMBLE_AND_SUBCOMMAND: u8 = 0x01;
const INPUT_SUBCOMMAND_REPLY: u8 = 0x21;
const SUBCOMMAND_SET_REPORT_MODE: u8 = 0x03;
const SUBCOMMAND_SET_HCI_STATE: u8 = 0x06;
const SUBCOMMAND_SET_PLAYER_LIGHTS: u8 = 0x30;
const SUBCOMMAND_ENABLE_VIBRATION: u8 = 0x48;
const FULL_REPORT_MODE: u8 = 0x30;
const NEUTRAL_RUMBLE: [u8; 4] = [0x00, 0x01, 0x40, 0x40];

pub struct SwitchDevice {
    label: &'static str,
}

impl SwitchDevice {
    pub const fn new(label: &'static str) -> Self {
        Self { label }
    }

    fn packet_counter(state: &Value) -> u8 {
        state
            .get("packet_counter")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u8
    }

    async fn send_subcommand(
        &self,
        transport: &mut dyn Transport,
        packet_counter: &mut u8,
        subcommand: u8,
        data: &[u8],
    ) -> Result<()> {
        let mut reports = transport.reports();
        for attempt in 0..3 {
            let report = Self::subcommand_report(*packet_counter, subcommand, data);
            *packet_counter = (*packet_counter + 1) & 0x0f;
            transport.send(&report).await?;
            match Self::wait_for_subcommand_reply(&mut reports, subcommand).await {
                Ok(()) => return Ok(()),
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

    async fn wait_for_subcommand_reply(reports: &mut ReportReceiver, subcommand: u8) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                match reports.recv().await {
                    Ok(report)
                        if report.first() == Some(&INPUT_SUBCOMMAND_REPLY)
                            && report.len() >= 15
                            && report[14] == subcommand =>
                    {
                        return if report[13] & 0x80 != 0 {
                            Ok(())
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
}

#[async_trait]
impl ControllerDevice for SwitchDevice {
    async fn sync(
        &self,
        transport: &mut dyn Transport,
        options: &ConnectOptions,
        state: &mut Value,
    ) -> Result<()> {
        let mut counter = 0;
        self.send_subcommand(
            transport,
            &mut counter,
            SUBCOMMAND_SET_REPORT_MODE,
            &[FULL_REPORT_MODE],
        )
        .await?;
        self.send_subcommand(
            transport,
            &mut counter,
            SUBCOMMAND_ENABLE_VIBRATION,
            &[0x01],
        )
        .await?;
        self.send_subcommand(
            transport,
            &mut counter,
            SUBCOMMAND_SET_PLAYER_LIGHTS,
            &[binary_player_leds(options.player_index)],
        )
        .await?;
        tracing::debug!(controller = self.label, "synchronized Switch controller");
        state["packet_counter"] = Value::from(counter);
        Ok(())
    }
    async fn set_player_led(
        &self,
        transport: &mut dyn Transport,
        slot: u8,
        state: &mut Value,
    ) -> Result<()> {
        let mut counter = Self::packet_counter(state);
        self.send_subcommand(
            transport,
            &mut counter,
            SUBCOMMAND_SET_PLAYER_LIGHTS,
            &[binary_player_leds(slot)],
        )
        .await?;
        state["packet_counter"] = Value::from(counter);
        Ok(())
    }
    async fn sleep(&self, transport: &mut dyn Transport, state: &mut Value) -> Result<()> {
        let mut counter = Self::packet_counter(state);
        tokio::time::sleep(Duration::from_millis(1_200)).await;
        let report = Self::subcommand_report(counter, SUBCOMMAND_SET_HCI_STATE, &[0x00]);
        counter = (counter + 1) & 0x0f;
        transport.send(&report).await?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        state["packet_counter"] = Value::from(counter);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn builds_counted_full_report_subcommand() {
        let report = SwitchDevice::subcommand_report(0x12, SUBCOMMAND_SET_REPORT_MODE, &[0x30]);
        assert_eq!(
            report[0..11],
            [
                0x01, 0x02, 0x00, 0x01, 0x40, 0x40, 0x00, 0x01, 0x40, 0x40, 0x03
            ]
        );
        assert_eq!(report[11], 0x30);
    }
}
