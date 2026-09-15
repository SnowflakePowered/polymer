//! Per-controller sensor calibration. Protocol reference:
//! https://github.com/nefarius/DsHidMini/blob/master/docs/MOTION.md
use crate::{Error, Result, Transport, transport::HidReportType};
use std::sync::{Arc, Mutex};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned, byteorder::big_endian::U16};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MotionPath {
    Hardware,
    Plain,
    Sixaxis,
}

#[derive(Clone, Copy, Debug)]
pub struct MotionCalibration {
    pub path: MotionPath,
    pub acceleration_zero: [u16; 3],
    pub acceleration_span: [u16; 3],
    pub gyro_zero: f32,
    pub cal_byte: u8,
    pub settling: bool,
}

#[derive(Clone, Debug, Default)]
pub struct MotionState(Arc<Mutex<Option<MotionCalibration>>>);

impl MotionState {
    pub fn new(calibration: MotionCalibration) -> Self {
        Self(Arc::new(Mutex::new(Some(calibration))))
    }

    pub(super) fn clear(&self) {
        *self.0.lock().unwrap_or_else(|error| error.into_inner()) = None;
    }
    pub fn snapshot(&self) -> Option<MotionCalibration> {
        *self.0.lock().unwrap_or_else(|error| error.into_inner())
    }
    pub(super) fn publish(&self, calibration: MotionCalibration) {
        *self.0.lock().unwrap_or_else(|error| error.into_inner()) = Some(calibration);
    }
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct FactoryPage {
    pairs: [[U16; 2]; 4],
}

impl MotionCalibration {
    pub(super) async fn read(transport: &mut dyn Transport) -> Result<Self> {
        let identification = transport.get_report(HidReportType::Feature, 1, 64).await?;
        tracing::debug!(data = ?identification, "DS3 identification response");
        let path = Self::identify(&identification)?;
        let mut select = [0; 49];
        select[0] = 0xef;
        select[5..8].copy_from_slice(&[3, 1, 0xa0]);
        transport
            .set_report(HidReportType::Feature, &select)
            .await?;
        let page = transport
            .get_report(HidReportType::Feature, 0xef, 64)
            .await?;
        tracing::debug!(data = ?page, "DS3 calibration page response");
        Self::parse(path, &page)
    }

    fn identify(data: &[u8]) -> Result<MotionPath> {
        if data.len() < 38 || !matches!(data[0], 0 | 1) || data[1] != 1 {
            return Err(Error::InvalidPacket("DS3 identification header"));
        }
        let fields = data
            .get(38..38 + usize::from(data[37]))
            .filter(|fields| !fields.is_empty())
            .ok_or(Error::InvalidPacket("DS3 calibration field list"))?;
        Ok(if fields.contains(&7) {
            MotionPath::Hardware
        } else if fields.starts_with(&[1, 2])
            || fields
                .get(1..)
                .is_some_and(|fields| fields.starts_with(&[1, 2]))
        {
            MotionPath::Plain
        } else {
            MotionPath::Sixaxis
        })
    }

    fn parse(path: MotionPath, data: &[u8]) -> Result<Self> {
        if data.len() < 33
            || !matches!(data[0], 0 | 0xef)
            || data[1] != 0xef
            || data[5..8] != [3, 1, 0xa0]
        {
            return Err(Error::InvalidPacket("DS3 EEPROM page header"));
        }
        let page = FactoryPage::ref_from_bytes(&data[17..33])
            .map_err(|_| Error::InvalidPacket("DS3 EEPROM page"))?;
        let pairs = page.pairs.map(|pair| pair.map(|value| value.get()));
        if pairs[..3]
            .iter()
            .any(|[zero, one_g]| *zero > 1023 || *one_g >= *zero)
            || pairs[3][0] > 1023
            || pairs[3][1] > 255
        {
            return Err(Error::InvalidPacket(
                "invalid DS3 factory sensor calibration",
            ));
        }
        Ok(Self {
            path,
            acceleration_zero: [pairs[0][0], pairs[1][0], pairs[2][0]],
            acceleration_span: [
                pairs[0][0] - pairs[0][1],
                pairs[1][0] - pairs[1][1],
                pairs[2][0] - pairs[2][1],
            ],
            gyro_zero: f32::from(pairs[3][0]),
            cal_byte: pairs[3][1] as u8,
            settling: path != MotionPath::Plain,
        })
    }

    pub(super) fn apply_output(&self, report: &mut [u8; 49]) {
        let offset = match self.path {
            MotionPath::Hardware => 6,
            MotionPath::Sixaxis => 4,
            MotionPath::Plain => return,
        };
        report[offset] = 0xff;
        report[offset + 1] = self.cal_byte;
    }
}

/// Conservative rest-only auto-zero: never use the moving long-term average
/// as a new zero. Four quiet blocks must agree before changing hardware trim.
pub(super) struct Tracker {
    pub calibration: MotionCalibration,
    settle: u8,
    samples: Vec<u16>,
    quiet: Vec<f32>,
}

impl Tracker {
    pub fn new(calibration: MotionCalibration) -> Self {
        Self {
            calibration,
            settle: 32,
            samples: Vec::with_capacity(16),
            quiet: Vec::with_capacity(4),
        }
    }

    pub fn observe(&mut self, raw: u16) -> Option<MotionCalibration> {
        if self.settle > 0 {
            self.settle -= 1;
            return None;
        }
        if !(204..=819).contains(&raw) {
            self.samples.clear();
            self.quiet.clear();
            return None;
        }
        self.samples.push(raw);
        if self.samples.len() != 16 {
            return None;
        }
        let average = self
            .samples
            .iter()
            .map(|value| f32::from(*value))
            .sum::<f32>()
            / 16.0;
        let minimum = f32::from(*self.samples.iter().min().unwrap());
        let maximum = f32::from(*self.samples.iter().max().unwrap());
        self.samples.clear();
        if (maximum - average).powi(2) + (average - minimum).powi(2) >= 10.0 {
            self.quiet.clear();
            return None;
        }
        self.quiet.push(average);
        if self.quiet.len() < 4 {
            return None;
        }
        let low = self.quiet.iter().copied().fold(f32::INFINITY, f32::min);
        let high = self.quiet.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let rest = self.quiet.iter().sum::<f32>() / 4.0;
        self.quiet.remove(0);
        if high - low >= 4.0 {
            return None;
        }
        let mut next = self.calibration;
        let steps = ((512.0 - rest) * 1024.0 / 0x6999 as f32).trunc() as i32;
        next.cal_byte = (i32::from(next.cal_byte) + steps).clamp(0, 255) as u8;
        // Do not predict the new ADC zero. Re-measure after the ACK and settling.
        next.settling = next.cal_byte != self.calibration.cal_byte;
        if !next.settling {
            next.gyro_zero = rest;
        }
        Some(next)
    }

    pub fn commit(&mut self, next: MotionCalibration) {
        if next.cal_byte != self.calibration.cal_byte {
            self.settle = 2;
            self.samples.clear();
            self.quiet.clear();
        }
        self.calibration = next;
    }
}
