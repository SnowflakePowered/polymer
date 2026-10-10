use super::{GuestPlatform, WindowsGuest, injection::InjectionLibrary, target::TargetProcess};
use crate::executable_architecture;
use polymer_hook::config;
use polymer_platform::{GraphicsHook, Guest, GuestLaunchRequest, InputHook};
use std::{ffi::OsString, path::Path};

impl GuestPlatform {
    pub fn launch(
        request: &GuestLaunchRequest,
        filesystem: &polymer_vfs::MountedFilesystem,
        input: polymer_ipc::input::host::InputIpcListener,
    ) -> std::io::Result<Box<dyn Guest>> {
        let process = Self::launch_process(request, filesystem)?;
        WindowsGuest::new(process, input).map(|guest| Box::new(guest) as Box<dyn Guest>)
    }

    fn launch_process(
        config: &GuestLaunchRequest,
        filesystem: &polymer_vfs::MountedFilesystem,
    ) -> std::io::Result<TargetProcess> {
        let binding = filesystem
            .filesystem()
            .compiled()
            .resolve(&config.executable)
            .map_err(std::io::Error::other)?
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "guest executable is not mounted",
                )
            })?;
        if !binding.permissions().can_execute() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "guest executable mount is not executable",
            ));
        }
        let command = filesystem.endpoint().project(config.executable.as_path());
        let command_line = std::iter::once(command.clone())
            .chain(config.arguments.iter().cloned())
            .collect::<Vec<_>>();
        let target_architecture = executable_architecture(Path::new(&command)).ok();
        let artifact_dir = target_architecture
            .map(|architecture| Self::artifact_dir(&config.artifact_root, architecture))
            .unwrap_or_else(|| config.artifact_root.clone());
        let vulkan_manifest = config
            .hooks
            .graphics
            .contains(&GraphicsHook::Vulkan)
            .then(|| {
                Self::create_vulkan_layer_manifest(
                    &artifact_dir.join("polymer_hook_vulkan_layer.dll"),
                )
            })
            .transpose()?;

        let mut environment = Self::hook_environment(config);
        let sdl2_runtime = artifact_dir.join("SDL2.dll");
        let sdl3_runtime = artifact_dir.join("SDL3.dll");
        if std::env::var_os("SDL_DYNAMIC_API").is_none() && sdl2_runtime.is_file() {
            environment.push((
                OsString::from("SDL_DYNAMIC_API"),
                sdl2_runtime.into_os_string(),
            ));
        }
        if std::env::var_os("SDL3_DYNAMIC_API").is_none() && sdl3_runtime.is_file() {
            environment.push((
                OsString::from("SDL3_DYNAMIC_API"),
                sdl3_runtime.into_os_string(),
            ));
        }
        if let Some(manifest) = &vulkan_manifest {
            environment.push((
                OsString::from("VK_LAYER_PATH"),
                manifest.path().as_os_str().to_owned(),
            ));
            environment.push((
                OsString::from("VK_INSTANCE_LAYERS"),
                OsString::from("VK_LAYER_POLYMER_swapchain"),
            ));
        }

        let libraries = Self::hook_libraries(&config.hooks.graphics, &config.hooks.input);
        let mut process = TargetProcess::launch(
            &command_line,
            &config.artifact_root,
            &environment,
            config.working_directory.as_deref(),
            config.isolate.then_some(&config.session),
            &libraries,
        )?;
        process.vulkan_manifest = vulkan_manifest;
        Ok(process)
    }

    fn hook_environment(runtime: &GuestLaunchRequest) -> Vec<(OsString, OsString)> {
        let mut environment = vec![
            (
                OsString::from(config::SESSION),
                OsString::from(&runtime.session),
            ),
            (OsString::from("SDL_JOYSTICK_HIDAPI"), OsString::from("0")),
            (
                OsString::from("SDL_JOYSTICK_DIRECTINPUT"),
                OsString::from("0"),
            ),
            (OsString::from("SDL_JOYSTICK_RAWINPUT"), OsString::from("0")),
            (OsString::from("SDL_JOYSTICK_WGI"), OsString::from("0")),
            (
                OsString::from("SDL_JOYSTICK_GAMEINPUT"),
                OsString::from("0"),
            ),
            (OsString::from("SDL_XINPUT_ENABLED"), OsString::from("0")),
        ];
        environment.extend(config::LOG_FILES.map(|(variable, file)| {
            (
                OsString::from(variable),
                runtime.artifact_root.join(file).into_os_string(),
            )
        }));
        environment.push((
            OsString::from(config::RENDER_CONFIGURATION),
            OsString::from(
                serde_json::to_string(&&runtime.render_configuration)
                    .expect("render configuration serializes"),
            ),
        ));
        environment.extend(runtime.environment.iter().cloned());
        environment.push((
            OsString::from(polymer_ipc::LocalSocketListener::DIRECTORY_ENV),
            polymer_ipc::LocalSocketListener::directory().into_os_string(),
        ));
        environment
    }

    fn create_vulkan_layer_manifest(layer_dll: &Path) -> std::io::Result<tempfile::TempDir> {
        if !layer_dll.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Vulkan hook is missing at {}", layer_dll.display()),
            ));
        }
        let directory = tempfile::Builder::new()
            .prefix("polymer-vulkan-layer-")
            .tempdir()?;
        let manifest = serde_json::json!({
            "file_format_version": "1.2.0",
            "layer": {
                "name": "VK_LAYER_POLYMER_swapchain",
                "type": "GLOBAL",
                "library_path": layer_dll,
                "api_version": "1.4.0",
                "implementation_version": "1",
                "description": "Polymer Vulkan swapchain layer",
                "functions": {
                    "vkNegotiateLoaderLayerInterfaceVersion":
                        "vkNegotiateLoaderLayerInterfaceVersion"
                },
                "enable_environment": {
                    "POLYMER_VULKAN_LAYER": "1"
                },
                "disable_environment": {
                    "DISABLE_POLYMER_VULKAN_LAYER": "1"
                }
            }
        });
        let file = std::fs::File::create(directory.path().join("polymer-layer.json"))?;
        serde_json::to_writer_pretty(file, &manifest).map_err(std::io::Error::other)?;
        Ok(directory)
    }

    fn hook_libraries(hooks: &[GraphicsHook], input_hooks: &[InputHook]) -> Vec<InjectionLibrary> {
        let mut libraries = Vec::new();
        if hooks
            .iter()
            .any(|hook| matches!(hook, GraphicsHook::D3d11 | GraphicsHook::D3d12))
        {
            libraries.push(InjectionLibrary::new(
                "DXGI",
                "polymer_hook_dxgi.dll",
                c"PolymerInstallDxgiHook",
            ));
        }
        if hooks.contains(&GraphicsHook::D3d11) {
            libraries.push(InjectionLibrary::new(
                "D3D11",
                "polymer_hook_d3d11.dll",
                c"PolymerInstallD3d11Hook",
            ));
        }
        if hooks.contains(&GraphicsHook::D3d12) {
            libraries.push(InjectionLibrary::new(
                "D3D12",
                "polymer_hook_d3d12.dll",
                c"PolymerInstallD3d12Hook",
            ));
        }
        if hooks.contains(&GraphicsHook::OpenGl) {
            libraries.push(InjectionLibrary::new(
                "OpenGL",
                "polymer_hook_opengl32.dll",
                c"PolymerInstallOpenGlHook",
            ));
        }
        libraries.push(InjectionLibrary::new(
            "DirectInput",
            "polymer_hook_dinput.dll",
            c"PolymerInstallDinputHook",
        ));
        libraries.push(InjectionLibrary::new(
            "Win32 input",
            "polymer_hook_win32.dll",
            c"PolymerInstallWin32Hook",
        ));
        if input_hooks.contains(&InputHook::BluetoothApiWin32) {
            libraries.push(InjectionLibrary::new(
                "Bluetooth HID API",
                "polymer_hook_bluetoothapi_win32.dll",
                c"PolymerInstallBluetoothApiWin32Hook",
            ));
        }
        if input_hooks.contains(&InputHook::Xinput) {
            libraries.push(InjectionLibrary::new(
                "XInput",
                "polymer_hook_xinput.dll",
                c"PolymerInstallXinputHook",
            ));
        }
        if input_hooks.contains(&InputHook::Sdl3) {
            libraries.push(InjectionLibrary::new(
                "SDL3",
                "polymer_hook_sdl3.dll",
                c"PolymerInstallSdl3Hook",
            ));
        }
        if input_hooks.contains(&InputHook::Sdl2) {
            libraries.push(InjectionLibrary::new(
                "SDL2",
                "polymer_hook_sdl2.dll",
                c"PolymerInstallSdl2Hook",
            ));
        }
        libraries
    }
}
