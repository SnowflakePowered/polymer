//! Adapt runtime configuration to the selected native guest backend.

use crate::{EmulatorLaunchConfig, LaunchError};
use polymer_platform::GuestLaunchRequest;

#[cfg(windows)]
pub(crate) use polymer_platform_windows::GuestPlatform;

#[cfg(target_os = "macos")]
pub(crate) use polymer_platform_macos::GuestPlatform;

impl EmulatorLaunchConfig {
    pub(crate) fn guest_request(
        &self,
        filesystem: &polymer_vfs::MountedFilesystem,
    ) -> Result<GuestLaunchRequest, LaunchError> {
        let command = if self.command.starts_with("file:") {
            url::Url::parse(&self.command)
                .map_err(|error| LaunchError::InvalidConfig(error.to_string()))?
                .to_file_path()
                .map_err(|()| LaunchError::InvalidConfig("guest file URL is not local".to_owned()))?
                .to_string_lossy()
                .into_owned()
        } else {
            self.command.clone()
        };
        let executable = filesystem.endpoint().unproject(&command).ok_or_else(|| {
            LaunchError::InvalidConfig("guest command is outside its mounted filesystem".to_owned())
        })?;
        Ok(GuestLaunchRequest {
            session: self.session.clone(),
            executable,
            arguments: self.arguments.clone(),
            environment: Vec::new(),
            artifact_root: self.artifact_root.clone(),
            hooks: self.hooks.clone(),
            isolate: self.isolate,
            render_configuration: self.render_configuration(),
        })
    }
}
