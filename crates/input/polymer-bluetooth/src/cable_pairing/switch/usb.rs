use crate::{Error, Result};
use std::time::{Duration, Instant};

pub(crate) trait InterruptReports: Send {
    fn write(&mut self, report: &[u8]) -> Result<()>;
    fn read(&mut self, report: &mut [u8], timeout_ms: i32) -> Result<usize>;
}

pub(super) struct Commands<D: InterruptReports> {
    device: D,
    counter: u8,
    initialized: bool,
    failed: bool,
}

impl<D: InterruptReports> Commands<D> {
    pub fn new(device: D) -> Self {
        Self {
            device,
            counter: 0,
            initialized: false,
            failed: false,
        }
    }

    pub fn initialize(&mut self) -> Result<()> {
        if self.failed {
            return Err(Error::InvalidPacket(
                "reopen Switch USB pairing after a failed command",
            ));
        }
        if self.initialized {
            return Ok(());
        }
        self.failed = true;
        // Nintendo USB handshake: normal baud, high baud, handshake again.
        for command in [2, 3, 2] {
            self.device.write(&[0x80, command])?;
            self.reply(0x81, command, None)?;
        }
        // Mark before writing so teardown also runs on partial write failure.
        self.initialized = true;
        self.device.write(&[0x80, 4])?;
        self.failed = false;
        Ok(())
    }

    pub fn subcommand(&mut self, command: u8, data: &[u8]) -> Result<Vec<u8>> {
        let stage = if command == 1 {
            Some(
                *data
                    .first()
                    .ok_or(Error::InvalidPacket("missing Switch pairing stage"))?,
            )
        } else {
            None
        };
        self.initialize()?;
        if data.len() > 38 {
            return Err(Error::InvalidPacket("oversized Switch USB subcommand"));
        }
        self.failed = true;
        let mut report = [0; 64];
        report[0] = 1;
        report[1] = self.counter;
        self.counter = (self.counter + 1) & 15;
        report[2..10].copy_from_slice(&[0, 1, 0x40, 0x40, 0, 1, 0x40, 0x40]);
        report[10] = command;
        report[11..11 + data.len()].copy_from_slice(data);
        self.device.write(&report)?;
        let response = self.reply(0x21, command, stage)?;
        self.failed = false;
        Ok(response)
    }

    fn reply(&mut self, kind: u8, command: u8, stage: Option<u8>) -> Result<Vec<u8>> {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let mut report = [0; 64];
            let length = self.device.read(&mut report, 100)?;
            let report = report
                .get(..length)
                .ok_or(Error::InvalidPacket("oversized Switch USB reply"))?;
            if report.first() != Some(&kind) {
                continue;
            }
            if kind == 0x81 {
                if report.get(1) == Some(&command) {
                    return Ok(report[2..].to_vec());
                }
                continue;
            }
            if report.get(14) != Some(&command) {
                continue;
            }
            if report[13] & 0x80 == 0 {
                return Err(Error::InvalidPacket("Switch USB subcommand rejected"));
            }
            let data = &report[15..];
            if stage.is_some_and(|stage| data.first() != Some(&stage)) {
                continue;
            }
            return Ok(data.to_vec());
        }
        Err(Error::Timeout(
            "Switch USB command reply; close other controller applications and retry",
        ))
    }

    pub fn read_spi(&mut self, address: u32, length: u8) -> Result<Vec<u8>> {
        if length == 0 || length > 29 {
            return Err(Error::InvalidPacket("invalid Switch SPI read length"));
        }
        let mut request = [0; 5];
        request[..4].copy_from_slice(&address.to_le_bytes());
        request[4] = length;
        let response = self.subcommand(0x10, &request)?;
        if response.get(..5) != Some(request.as_slice()) || response.len() < 5 + usize::from(length)
        {
            self.failed = true;
            return Err(Error::InvalidPacket(
                "Switch SPI read response did not match",
            ));
        }
        Ok(response[5..5 + usize::from(length)].to_vec())
    }
}

impl<D: InterruptReports> Drop for Commands<D> {
    fn drop(&mut self) {
        if self.initialized {
            // Restore timeout/BT fallback, including after a failed handshake.
            if let Err(error) = self.device.write(&[0x80, 5]) {
                tracing::warn!(%error, "failed to release Switch USB-only mode; unplug the cable");
            }
        }
    }
}
