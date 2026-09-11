use polymer_bluetooth::{Error, Result, cable_pairing::CableDeviceIdentity};

#[cfg(windows)]
mod windows;

pub struct DriverSwitch;

impl DriverSwitch {
    pub async fn switch(device: CableDeviceIdentity) -> Result<String> {
        if device.uses_dshidmini() {
            return Err(Error::Unsupported(
                "DsHidMini controllers use IPC pairing without a driver switch",
            ));
        }
        let path = device.path().to_string_lossy().into_owned();
        tokio::task::spawn_blocking(move || Self::switch_path(&path))
            .await
            .map_err(|error| Error::Platform(format!("driver switch task failed: {error}")))?
    }

    /// Uses the current process token. Never launches an elevated process,
    /// requests UAC consent, or changes every device sharing a hardware ID.
    #[cfg(windows)]
    pub fn switch_path(path: &str) -> Result<String> {
        windows::DeviceSet::switch(path)
    }

    #[cfg(not(windows))]
    pub fn switch_path(_: &str) -> Result<String> {
        Err(Error::Unsupported(
            "WinUSB driver switching is Windows-only",
        ))
    }
}
