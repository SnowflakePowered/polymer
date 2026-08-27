use std::{fmt, str::FromStr};

use crate::{Error, Result};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransportKind {
    Ble,
    BrEdr,
}

#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize,
)]
pub enum AddressKind {
    #[default]
    Public,
    Random,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct DeviceAddr {
    bytes: [u8; 6],
    kind: AddressKind,
}

impl DeviceAddr {
    pub const fn new(bytes: [u8; 6], kind: AddressKind) -> Self {
        Self { bytes, kind }
    }
    pub const fn public(bytes: [u8; 6]) -> Self {
        Self::new(bytes, AddressKind::Public)
    }
    pub const fn bytes(self) -> [u8; 6] {
        self.bytes
    }
    pub const fn kind(self) -> AddressKind {
        self.kind
    }

    /// HCI encodes Bluetooth addresses least-significant byte first.
    pub(crate) fn hci_bytes(self) -> [u8; 6] {
        let mut bytes = self.bytes;
        bytes.reverse();
        bytes
    }

    pub(crate) fn from_hci_bytes(mut bytes: [u8; 6]) -> Self {
        bytes.reverse();
        Self::public(bytes)
    }
}

impl fmt::Display for DeviceAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, byte) in self.bytes.iter().enumerate() {
            if index != 0 {
                f.write_str(":")?;
            }
            write!(f, "{byte:02X}")?;
        }
        Ok(())
    }
}

impl FromStr for DeviceAddr {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        let parts: Vec<_> = value.split([':', '-']).collect();
        if parts.len() != 6 {
            return Err(Error::InvalidAddress(value.into()));
        }
        let mut bytes = [0; 6];
        for (slot, part) in bytes.iter_mut().zip(parts) {
            *slot =
                u8::from_str_radix(part, 16).map_err(|_| Error::InvalidAddress(value.into()))?;
        }
        Ok(Self::public(bytes))
    }
}
