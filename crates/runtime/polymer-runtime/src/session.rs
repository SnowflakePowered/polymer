//! Application-level ownership of one emulator process session.

use crate::{AudioState, EmulatorInstance, EmulatorLaunchConfig, InstanceState, NormalizedVolume};

pub fn filesystem_host() -> impl polymer_vfs::FilesystemHost {
    #[cfg(windows)]
    let host = polymer_vfs_winfsp::WinFspFilesystemHost::default();
    #[cfg(target_os = "macos")]
    let host = polymer_vfs_fskit::FsKitFilesystemHost::new("gg.polymer.filesystem");
    #[cfg(not(any(windows, target_os = "macos")))]
    let host = polymer_vfs::UnsupportedFilesystemHost;
    host
}

/// Validates and mounts a plugin-prepared filesystem for an emulator launch.
pub fn mount_prepared_filesystem(
    host: &dyn polymer_vfs::FilesystemHost,
    prepared: polymer_vfs::PreparedFilesystemLaunch,
    command: &str,
) -> Result<polymer_vfs::MountedFilesystem, String> {
    prepared
        .validate_command(command)
        .map_err(|error| error.to_string())?;
    if !host.supported() {
        return Err(
            "the plugin prepared a virtual filesystem, but no filesystem host is available"
                .to_owned(),
        );
    }
    host.mount(prepared.endpoint().clone(), prepared.filesystem().clone())
        .map_err(|error| error.to_string())
}

struct ActiveSession {
    token: u32,
    instance: EmulatorInstance,
    audio: AudioState,
    filesystem: polymer_vfs::MountedFilesystem,
}

/// Owns the native emulator lifecycle independently from shell presentation.
#[derive(Default)]
pub struct SessionController {
    active: Option<ActiveSession>,
}

impl SessionController {
    pub fn is_active(&self, token: u32) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.token == token)
    }

    pub fn active_token(&self) -> Option<u32> {
        self.active.as_ref().map(|active| active.token)
    }

    pub fn has_active_session(&self) -> bool {
        self.active.is_some()
    }

    pub fn instance(&self) -> Option<&EmulatorInstance> {
        self.active.as_ref().map(|active| &active.instance)
    }

    pub fn launch(
        &mut self,
        token: u32,
        config: EmulatorLaunchConfig,
        filesystem: polymer_vfs::MountedFilesystem,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<(), String> {
        self.launch_with_input(token, config, filesystem, wake, |_| Ok(()))
    }

    pub fn launch_with_input(
        &mut self,
        token: u32,
        config: EmulatorLaunchConfig,
        filesystem: polymer_vfs::MountedFilesystem,
        wake: impl Fn() + Send + Sync + 'static,
        configure_input: impl FnOnce(polymer_ipc::input::host::InputIpcListener) -> Result<(), String>,
    ) -> Result<(), String> {
        if self.active.is_some() {
            return Err("another emulator instance is active".to_owned());
        }
        let instance =
            match EmulatorInstance::launch_with_input(config, &filesystem, wake, configure_input) {
                Ok(instance) => instance,
                Err(error) => {
                    let mut message = error.to_string();
                    if let Err(finalization) =
                        filesystem.finish(polymer_vfs::FinalizationReason::LaunchFailed)
                    {
                        message.push_str("; filesystem finalization failed: ");
                        message.push_str(&finalization.to_string());
                    }
                    return Err(message);
                }
            };
        let audio = instance.audio_state();
        self.active = Some(ActiveSession {
            token,
            instance,
            audio,
            filesystem,
        });
        Ok(())
    }

    pub fn close(&mut self, token: u32) -> bool {
        let Some(active) = self.active.as_mut().filter(|active| active.token == token) else {
            return false;
        };
        active.instance.close();
        true
    }

    pub fn set_volume(&self, token: u32, volume: f64) -> bool {
        let Some(volume) = NormalizedVolume::new(volume as f32) else {
            return false;
        };
        let Some(active) = self.active.as_ref().filter(|active| active.token == token) else {
            return false;
        };
        active.instance.set_volume(volume);
        true
    }

    pub fn set_muted(&self, token: u32, muted: bool) -> bool {
        let Some(active) = self.active.as_ref().filter(|active| active.token == token) else {
            return false;
        };
        active.instance.set_muted(muted);
        true
    }

    pub fn audio_change(&mut self, force: bool) -> Option<(u32, AudioState)> {
        let active = self.active.as_mut()?;
        let audio = active.instance.audio_state();
        if !force && active.audio == audio {
            return None;
        }
        active.audio = audio;
        Some((active.token, audio))
    }

    pub fn has_exited(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| matches!(active.instance.state(), InstanceState::Exited(_)))
    }

    pub fn finish(
        &mut self,
        reason: polymer_vfs::FinalizationReason,
    ) -> Result<Option<u32>, String> {
        let Some(active) = self.active.take() else {
            return Ok(None);
        };
        let token = active.token;
        drop(active.instance);
        let filesystem = active.filesystem;
        if let Ok(path) = polymer_vfs::VfsPathBuf::new("/User/Logs/dolphin.log") {
            let projected = filesystem.endpoint().project(path.as_path());
            if let Ok(log) = std::fs::read_to_string(projected) {
                tracing::info!("polymer: Dolphin log before unmount:\n{log}");
            }
        }
        filesystem
            .finish(reason)
            .map_err(|error| error.to_string())?;
        Ok(Some(token))
    }
}

impl Drop for SessionController {
    fn drop(&mut self) {
        if let Some(active) = self.active.as_mut() {
            active.instance.close();
        }
        let _ = self.finish(polymer_vfs::FinalizationReason::Cancelled);
    }
}
