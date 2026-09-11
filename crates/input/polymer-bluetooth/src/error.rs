use std::fmt;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    Io(std::io::Error),
    Bluetooth(btleplug::Error),
    Usb(nusb::Error),
    UsbTransfer(nusb::transfer::TransferError),
    UsbConfiguration(nusb::ActiveConfigurationError),
    AdapterNotFound,
    InvalidAddress(String),
    AmbiguousProfile(crate::DeviceAddr),
    MissingProfile(String),
    InvalidPacket(&'static str),
    Unsupported(&'static str),
    Firmware(String),
    Store(String),
    Platform(String),
    HciStatus { operation: &'static str, status: u8 },
    L2cap(String),
    Backoff(std::time::Duration),
    ChannelClosed,
    Timeout(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Bluetooth(error) => write!(f, "OS Bluetooth error: {error}"),
            Self::Usb(error) => write!(f, "USB error: {error}"),
            Self::UsbTransfer(error) => write!(f, "USB transfer error: {error}"),
            Self::UsbConfiguration(error) => write!(f, "USB configuration error: {error}"),
            Self::AdapterNotFound => f.write_str("no accessible dedicated Bluetooth USB adapter was found; on Windows bind it to WinUSB"),
            Self::InvalidAddress(value) => write!(f, "invalid Bluetooth address: {value}"),
            Self::AmbiguousProfile(address) => write!(f, "multiple controller profiles match {address} with equal confidence"),
            Self::MissingProfile(id) => write!(f, "controller profile {id} is not registered"),
            Self::InvalidPacket(reason) => write!(f, "invalid Bluetooth packet: {reason}"),
            Self::Unsupported(reason) => write!(f, "unsupported Bluetooth operation: {reason}"),
            Self::Firmware(reason) => write!(f, "Bluetooth controller firmware error: {reason}"),
            Self::Store(reason) => write!(f, "Bluetooth controller store error: {reason}"),
            Self::Platform(reason) => write!(f, "Bluetooth platform error: {reason}"),
            Self::HciStatus { operation, status } => write!(f, "{operation} failed with HCI status 0x{status:02x}"),
            Self::L2cap(reason) => write!(f, "L2CAP error: {reason}"),
            Self::Backoff(delay) => write!(f, "Bluetooth reconnect is cooling down; retry in {:.1}s", delay.as_secs_f32()),
            Self::ChannelClosed => f.write_str("Bluetooth worker stopped"),
            Self::Timeout(operation) => write!(f, "timed out while waiting for {operation}"),
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    /// True when BLE discovery merely observed a sleeping or stale cached
    /// peripheral. This is normal wake-listener state, not a failed hardware
    /// connection attempt, and must not increase controller cooldown.
    pub fn is_ble_device_absent(&self) -> bool {
        matches!(
            self,
            Self::Timeout("BLE advertisement") | Self::Bluetooth(btleplug::Error::DeviceNotFound)
        )
    }
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<btleplug::Error> for Error {
    fn from(value: btleplug::Error) -> Self {
        Self::Bluetooth(value)
    }
}

impl From<nusb::Error> for Error {
    fn from(value: nusb::Error) -> Self {
        Self::Usb(value)
    }
}

impl From<nusb::transfer::TransferError> for Error {
    fn from(value: nusb::transfer::TransferError) -> Self {
        Self::UsbTransfer(value)
    }
}

impl From<nusb::ActiveConfigurationError> for Error {
    fn from(value: nusb::ActiveConfigurationError) -> Self {
        Self::UsbConfiguration(value)
    }
}

impl<M> From<kameo::error::SendError<M, Error>> for Error {
    fn from(error: kameo::error::SendError<M, Error>) -> Self {
        use kameo::error::SendError;
        match error {
            SendError::HandlerError(error) => error,
            SendError::MailboxFull(_) | SendError::Timeout(Some(_)) => {
                Self::Timeout("Bluetooth command queue")
            }
            SendError::Timeout(None) => Self::Timeout("Bluetooth command response"),
            _ => Self::ChannelClosed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_sleeping_ble_devices_without_hiding_real_failures() {
        assert!(Error::Timeout("BLE advertisement").is_ble_device_absent());
        assert!(Error::Bluetooth(btleplug::Error::DeviceNotFound).is_ble_device_absent());
        assert!(!Error::Bluetooth(btleplug::Error::PermissionDenied).is_ble_device_absent());
    }
}
