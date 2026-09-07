use crate::{EmulatorLaunchConfig, GraphicsHook, InputHook, LaunchError};
use polymer_hook::config;
use polymer_platform_windows::{
    InjectionLibrary, TargetProcess, architecture_artifact_dir, executable_architecture,
};
use std::{ffi::OsString, path::Path};

pub(crate) fn launch(config: &EmulatorLaunchConfig) -> Result<TargetProcess, LaunchError> {
    let command = file_url_or_path(&config.command)?;
    let command_line = std::iter::once(command.clone())
        .chain(config.arguments.iter().cloned())
        .collect::<Vec<_>>();
    let target_architecture = executable_architecture(Path::new(&command)).ok();
    let artifact_dir = target_architecture
        .map(|architecture| architecture_artifact_dir(&config.artifact_root, architecture))
        .unwrap_or_else(|| config.artifact_root.clone());
    let vulkan_manifest = config
        .hooks
        .graphics
        .contains(&GraphicsHook::Vulkan)
        .then(|| create_vulkan_layer_manifest(&artifact_dir.join("polymer_hook_vulkan_layer.dll")))
        .transpose()?;

    let mut environment = hook_environment(config);
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

    let libraries = hook_libraries(&config.hooks.graphics, &config.hooks.input, config.isolate);
    let mut process = TargetProcess::launch(
        &command_line,
        &config.artifact_root,
        &environment,
        config.isolate.then_some(&config.session),
        &libraries,
    )?;
    if let Some(manifest) = vulkan_manifest {
        process.retain(manifest);
    }
    Ok(process)
}

fn file_url_or_path(value: &str) -> Result<String, LaunchError> {
    if !value.starts_with("file:") {
        return Ok(value.to_owned());
    }
    let url = url::Url::parse(value).map_err(|error| {
        LaunchError::InvalidConfig(format!("invalid file URL '{value}': {error}"))
    })?;
    url.to_file_path()
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|()| LaunchError::InvalidConfig(format!("file URL is not local: {value}")))
}

fn hook_environment(runtime: &EmulatorLaunchConfig) -> Vec<(OsString, OsString)> {
    let mut environment = vec![
        (
            OsString::from(config::SESSION),
            OsString::from(&runtime.session),
        ),
        (
            OsString::from(config::LAYER_LOG_FILE),
            runtime
                .artifact_root
                .join("polymer-layer.log")
                .into_os_string(),
        ),
        (
            OsString::from(config::DINPUT_LOG_FILE),
            runtime
                .artifact_root
                .join("polymer-dinput.log")
                .into_os_string(),
        ),
        (
            OsString::from(config::WIN32_LOG_FILE),
            runtime
                .artifact_root
                .join("polymer-win32.log")
                .into_os_string(),
        ),
        (
            OsString::from(config::SDL_LOG_FILE),
            runtime
                .artifact_root
                .join("polymer-sdl.log")
                .into_os_string(),
        ),
        (
            OsString::from(config::DXGI_LOG_FILE),
            runtime
                .artifact_root
                .join("polymer-dxgi.log")
                .into_os_string(),
        ),
        (
            OsString::from(config::D3D11_LOG_FILE),
            runtime
                .artifact_root
                .join("polymer-d3d11.log")
                .into_os_string(),
        ),
        (
            OsString::from(config::D3D12_LOG_FILE),
            runtime
                .artifact_root
                .join("polymer-d3d12.log")
                .into_os_string(),
        ),
        (
            OsString::from(config::OPENGL_LOG_FILE),
            runtime
                .artifact_root
                .join("polymer-opengl.log")
                .into_os_string(),
        ),
        (
            OsString::from(config::BLUETOOTH_API_LOG_FILE),
            runtime
                .artifact_root
                .join("polymer-bluetooth-api.log")
                .into_os_string(),
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
    environment.push((
        OsString::from(config::RENDER_CONFIGURATION),
        OsString::from(
            serde_json::to_string(&runtime.render_configuration())
                .expect("render configuration serializes"),
        ),
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

fn hook_libraries(
    hooks: &[GraphicsHook],
    input_hooks: &[InputHook],
    isolate: bool,
) -> Vec<InjectionLibrary> {
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
    if input_hooks.contains(&InputHook::Sdl3) {
        libraries.push(InjectionLibrary::new(
            "SDL3",
            "polymer_hook_sdl3.dll",
            c"PolymerInstallSdl3Hook",
        ));
    }
    if isolate {
        libraries.push(InjectionLibrary::new(
            "UI",
            "polymer_hook_ui.dll",
            c"PolymerInstallUiHook",
        ));
    }
    libraries.push(InjectionLibrary::new(
        "SDL2",
        "polymer_hook_sdl2.dll",
        c"PolymerInstallSdl2Hook",
    ));
    libraries
}
