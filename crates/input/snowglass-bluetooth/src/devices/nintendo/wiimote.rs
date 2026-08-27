//! Wii Remote connection-time initialization.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

use crate::devices::{ConnectOptions, ControllerDevice, binary_player_leds};
use crate::{Result, Transport};

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

#[derive(Default)]
pub struct WiimoteDevice;

#[async_trait]
impl ControllerDevice for WiimoteDevice {
    async fn sync(
        &self,
        transport: &mut dyn Transport,
        options: &ConnectOptions,
        _state: &mut Value,
    ) -> Result<()> {
        self.set_player_led(transport, options.player_index, false)
            .await?;
        self.write_i2c(
            transport,
            I2C_MOTION_PLUS_INACTIVE,
            MOTION_PLUS_INIT_ADDRESS,
            &[0x55],
        )
        .await?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        self.write_i2c(
            transport,
            I2C_MOTION_PLUS_INACTIVE,
            MOTION_PLUS_MODE_ADDRESS,
            &[MOTION_PLUS_DISABLED_PASSTHROUGH],
        )
        .await?;
        tokio::time::sleep(Duration::from_millis(500)).await;
        transport.send(&[IR_ENABLE_2_ID, 0x04]).await?;
        transport.send(&[IR_ENABLE_ID, 0x04]).await?;
        self.write_i2c(
            transport,
            I2C_CAMERA,
            0x0000,
            &[0x02, 0x00, 0x00, 0x71, 0x01, 0x00, 0xaa, 0x00, 0x64],
        )
        .await?;
        self.write_i2c(transport, I2C_CAMERA, 0x001a, &[0x63, 0x03])
            .await?;
        self.write_i2c(transport, I2C_CAMERA, 0x0033, &[0x01])
            .await?;
        self.write_i2c(transport, I2C_CAMERA, 0x0030, &[0x08])
            .await?;
        transport
            .send(&[REPORT_MODE_ID, CONTINUOUS, REPORT_CORE_ACCEL_IR10_EXT6])
            .await
    }

    async fn set_player_led(
        &self,
        transport: &mut dyn Transport,
        slot: u8,
        _state: &mut Value,
    ) -> Result<()> {
        self.set_player_led(transport, slot, false).await
    }
}

impl WiimoteDevice {
    async fn set_player_led(
        &self,
        transport: &mut dyn Transport,
        slot: u8,
        rumble: bool,
    ) -> Result<()> {
        let led = binary_player_leds(slot) << 4;
        transport
            .send(&[LED_REPORT_ID, led | u8::from(rumble)])
            .await
    }

    async fn write_i2c(
        &self,
        transport: &mut dyn Transport,
        slave: u8,
        address: u16,
        data: &[u8],
    ) -> Result<()> {
        debug_assert!(data.len() <= 16);
        let mut report = [0u8; 22];
        report[0] = WRITE_DATA_ID;
        report[1] = 0x04;
        report[2] = slave << 1;
        report[3..5].copy_from_slice(&address.to_be_bytes());
        report[5] = data.len() as u8;
        report[6..6 + data.len()].copy_from_slice(data);
        transport.send(&report).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn displays_one_based_player_index_in_binary() {
        let report = |slot: u8, rumble| {
            let led = binary_player_leds(slot) << 4;
            [LED_REPORT_ID, led | u8::from(rumble)]
        };
        assert_eq!(report(0, false), [0x11, 0x10]);
        assert_eq!(report(2, true), [0x11, 0x31]);
        assert_eq!(report(3, false), [0x11, 0x40]);
    }
}
