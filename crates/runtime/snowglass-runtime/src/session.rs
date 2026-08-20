//! Application-level ownership of one emulator process session.

use crate::{AudioState, EmulatorInstance, EmulatorLaunchConfig, InstanceState, NormalizedVolume};

/// Validates and mounts a plugin-prepared filesystem for an emulator launch.
pub fn mount_prepared_filesystem(
    host: &dyn snowglass_vfs::FilesystemHost,
    prepared: snowglass_vfs::PreparedFilesystemLaunch,
    command: &str,
) -> Result<snowglass_vfs::MountedFilesystem, String> {
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
    filesystem: Option<snowglass_vfs::MountedFilesystem>,
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
        mut filesystem: Option<snowglass_vfs::MountedFilesystem>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<(), String> {
        if self.active.is_some() {
            return Err("another emulator instance is active".to_owned());
        }
        let instance = match EmulatorInstance::launch_with_waker(config, wake) {
            Ok(instance) => instance,
            Err(error) => {
                if let Some(filesystem) = filesystem.take() {
                    filesystem.finish(snowglass_vfs::FinalizationReason::LaunchFailed);
                }
                return Err(error.to_string());
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

    pub fn finish(&mut self, reason: snowglass_vfs::FinalizationReason) -> Option<u32> {
        let mut active = self.active.take()?;
        let token = active.token;
        drop(active.instance);
        if let Some(filesystem) = active.filesystem.take() {
            filesystem.finish(reason);
        }
        Some(token)
    }
}

impl Drop for SessionController {
    fn drop(&mut self) {
        if let Some(active) = self.active.as_mut() {
            active.instance.close();
        }
        self.finish(snowglass_vfs::FinalizationReason::Cancelled);
    }
}
