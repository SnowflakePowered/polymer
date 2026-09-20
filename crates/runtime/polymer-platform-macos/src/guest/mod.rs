mod process;
use crate::SystemAudioState;
use polymer_ipc::{Message, input::host::InputIpcListener};
use polymer_platform::*;
use process::Process;
use std::{io, path::Path, process::Command};

pub struct GuestPlatform;
struct MacGuest {
    process: Process,
    architecture: ProcessArchitecture,
    input: InputIpcListener,
}
impl GuestPlatform {
    pub fn launch(
        request: &GuestLaunchRequest,
        filesystem: &polymer_vfs::MountedFilesystem,
        input: InputIpcListener,
    ) -> io::Result<Box<dyn Guest>> {
        let binding = filesystem
            .filesystem()
            .compiled()
            .resolve(&request.executable)
            .map_err(io::Error::other)?
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "guest executable is not mounted")
            })?;
        if !binding.permissions().can_execute() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "guest mount is not executable",
            ));
        }
        request
            .render_configuration
            .validate()
            .map_err(io::Error::other)?;
        if request
            .hooks
            .graphics
            .iter()
            .any(|hook| !matches!(hook, GraphicsHook::Metal | GraphicsHook::Vulkan))
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "macOS capture supports Metal and MoltenVK",
            ));
        }
        if request
            .hooks
            .input
            .iter()
            .any(|hook| !matches!(hook, InputHook::Sdl3))
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "macOS controller hooks support SDL3 only",
            ));
        }
        let executable = filesystem.endpoint().project(request.executable.as_path());
        let architecture = Self::architecture(Path::new(&executable))?;
        let artifacts = request.artifact_root.join(architecture.executable_suffix());
        let artifacts = if artifacts.is_dir() {
            artifacts
        } else {
            request.artifact_root.clone()
        };
        let mut libraries = Vec::new();
        if !request.hooks.graphics.is_empty() {
            libraries.push(artifacts.join("libpolymer_hook_metal.dylib"));
        }
        libraries.push(artifacts.join("libpolymer_hook_appkit.dylib"));
        let libraries = libraries
            .iter()
            .map(|path| {
                path.canonicalize()
                    .map(|p| p.to_string_lossy().into_owned())
            })
            .collect::<io::Result<Vec<_>>>()?;
        let mut command = Command::new(&executable);
        command
            .args(&request.arguments)
            .envs(request.environment.iter().cloned())
            .env("DYLD_INSERT_LIBRARIES", libraries.join(":"))
            .env("POLYMER_SESSION", &request.session)
            .env(
                "POLYMER_RENDER_CONFIGURATION",
                serde_json::to_string(&request.render_configuration)?,
            )
            .env(
                "POLYMER_DESKTOP_ISOLATION",
                if request.isolate { "1" } else { "0" },
            )
            .current_dir(Path::new(&executable).parent().unwrap());
        if request.hooks.input.contains(&InputHook::Sdl3) {
            command
                .env(
                    "SDL3_DYNAMIC_API",
                    artifacts
                        .join("libpolymer_hook_sdl3.dylib")
                        .canonicalize()?,
                )
                .env(
                    "POLYMER_SDL3_RUNTIME",
                    artifacts.join("libSDL3.dylib").canonicalize()?,
                );
        }
        let process = Process::spawn(&mut command)?;
        Ok(Box::new(MacGuest {
            process,
            architecture,
            input,
        }))
    }
    fn architecture(path: &Path) -> io::Result<ProcessArchitecture> {
        // lipo understands thin and universal Mach-O, including swapped headers.
        let output = Command::new("/usr/bin/lipo")
            .arg("-archs")
            .arg(path)
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other("guest is not a Mach-O executable"));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        if text.split_whitespace().any(|arch| arch == "arm64") {
            Ok(ProcessArchitecture::Arm64)
        } else if text.split_whitespace().any(|arch| arch == "x86_64") {
            Ok(ProcessArchitecture::X86_64)
        } else {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "guest architecture is unsupported",
            ))
        }
    }
}
impl Guest for MacGuest {
    fn id(&self) -> ProcessId {
        ProcessId(self.process.id())
    }
    fn architecture(&self) -> ProcessArchitecture {
        self.architecture
    }
    fn subscribe_exit(&self) -> io::Result<ProcessExitReceiver> {
        Ok(self.process.subscribe())
    }
    fn window_dimensions(&self, window: GuestWindowId) -> io::Result<ResizeDimensions> {
        crate::window::GuestEvents::dimensions(self.process.id(), window)
    }
    fn resize_window(&self, window: GuestWindowId, size: ResizeDimensions) -> io::Result<()> {
        if size.width == 0 || size.height == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "window dimensions must be nonzero",
            ));
        }
        self.input
            .send_control(Message::ResizeWindow(polymer_protocol::ResizeWindow {
                target_window: window.0,
                width: size.width,
                height: size.height,
            }))
            .then_some(())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotConnected,
                    "guest AppKit input is unavailable",
                )
            })
    }
    fn send_input(&self, input: GuestInput) -> io::Result<()> {
        match input {
            GuestInput::Host(event) => {
                self.input.publish_host(event);
                Ok(())
            }
            action @ GuestInput::Hotkey { .. } => {
                crate::window::GuestEvents::send(self.process.id(), action)
            }
        }
    }
    fn audio_state(&self) -> io::Result<AudioState> {
        SystemAudioState::read()
    }
    fn set_volume(&self, volume: NormalizedVolume) -> io::Result<()> {
        SystemAudioState::set_volume(volume)
    }
    fn set_muted(&self, muted: bool) -> io::Result<()> {
        SystemAudioState::set_muted(muted)
    }
    fn terminate_tree_and_wait(&mut self) -> io::Result<()> {
        self.process.terminate()
    }
}
