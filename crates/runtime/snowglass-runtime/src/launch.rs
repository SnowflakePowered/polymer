use crate::{EmulatorLaunchConfig, HookBackend, LaunchError};
use snowglass_hook::config;
use snowglass_platform_windows::{
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
        .contains(&HookBackend::Vulkan)
        .then(|| {
            create_vulkan_layer_manifest(&artifact_dir.join("snowglass_hook_vulkan_layer.dll"))
        })
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
            OsString::from("VK_LAYER_SNOWGLASS_capture"),
        ));
    }

    let libraries = hook_libraries(&config.hooks, &config.input_hooks, config.isolate);
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
            OsString::from(config::LAYOUTS),
            OsString::from(runtime.gamepad_layouts.join(",")),
        ),
        (
            OsString::from(config::SDL_GAMEPADS),
            OsString::from(runtime.gamepad_layouts.len().to_string()),
        ),
        (
            OsString::from(config::LAYER_LOG_FILE),
            runtime
                .artifact_root
                .join("snowglass-layer.log")
                .into_os_string(),
        ),
        (
            OsString::from(config::DINPUT_LOG_FILE),
            runtime
                .artifact_root
                .join("snowglass-dinput.log")
                .into_os_string(),
        ),
        (
            OsString::from(config::WIN32_LOG_FILE),
            runtime
                .artifact_root
                .join("snowglass-win32.log")
                .into_os_string(),
        ),
        (
            OsString::from(config::SDL_LOG_FILE),
            runtime
                .artifact_root
                .join("snowglass-sdl.log")
                .into_os_string(),
        ),
        (
            OsString::from(config::DXGI_LOG_FILE),
            runtime
                .artifact_root
                .join("snowglass-dxgi.log")
                .into_os_string(),
        ),
        (
            OsString::from(config::D3D11_LOG_FILE),
            runtime
                .artifact_root
                .join("snowglass-d3d11.log")
                .into_os_string(),
        ),
        (
            OsString::from(config::D3D12_LOG_FILE),
            runtime
                .artifact_root
                .join("snowglass-d3d12.log")
                .into_os_string(),
        ),
        (
            OsString::from(config::OPENGL_LOG_FILE),
            runtime
                .artifact_root
                .join("snowglass-opengl.log")
                .into_os_string(),
        ),
        (
            OsString::from(config::GDI_LOG_FILE),
            runtime
                .artifact_root
                .join("snowglass-gdi.log")
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
    if runtime
        .input_hooks
        .contains(&crate::InputHookBackend::BluetoothApiWin32)
    {
        let pipes = (0..4u8)
            .map(|port| {
                let active = runtime
                    .bluetooth_hid_pipes
                    .iter()
                    .find(|pipe| pipe.port == port);
                serde_json::json!({
                    "sourceId": active.map_or("", |pipe| pipe.source_id.as_str()),
                    "port": port,
                    "productId": active.map_or(0x0306, |pipe| pipe.product_id),
                    "pipe": format!(r"\\.\pipe\{}-wiimote-{}", runtime.session, port),
                })
            })
            .collect::<Vec<_>>();
        environment.push((
            OsString::from(config::BLUETOOTH_HID_PIPES),
            OsString::from(serde_json::to_string(&pipes).expect("pipe configuration serializes")),
        ));
    }
    if runtime.tracking_enabled {
        environment.push((
            OsString::from(config::TRACK_RENDER_TARGET),
            OsString::from("1"),
        ));
        environment.push((
            OsString::from(config::TRACK_RENDER_TARGETS),
            OsString::from(
                runtime
                    .tracked_surfaces
                    .iter()
                    .map(|surface| surface.render_target_index.to_string())
                    .collect::<Vec<_>>()
                    .join(","),
            ),
        ));
        if runtime
            .tracked_surfaces
            .iter()
            .any(|surface| surface.window_pattern.is_some())
        {
            let patterns = runtime
                .tracked_surfaces
                .iter()
                .map(|surface| surface.window_pattern.as_deref())
                .collect::<Vec<_>>();
            environment.push((
                OsString::from(config::TRACK_RENDER_TARGET_SURFACE_PATTERNS),
                OsString::from(
                    serde_json::to_string(&patterns).expect("surface patterns always serialize"),
                ),
            ));
        }
    }
    if runtime.tracking_enabled && runtime.graph_tracking {
        environment.push((OsString::from(config::GRAPH_TRACKING), OsString::from("1")));
    }
    if runtime.force_keyed_mutex {
        environment.push((
            OsString::from(config::FORCE_KEYED_MUTEX),
            OsString::from("1"),
        ));
    }
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
        .prefix("snowglass-vulkan-layer-")
        .tempdir()?;
    let manifest = serde_json::json!({
        "file_format_version": "1.2.0",
        "layer": {
            "name": "VK_LAYER_SNOWGLASS_capture",
            "type": "GLOBAL",
            "library_path": layer_dll,
            "api_version": "1.4.0",
            "implementation_version": "1",
            "description": "Snowglass Vulkan capture layer",
            "functions": {
                "vkNegotiateLoaderLayerInterfaceVersion":
                    "vkNegotiateLoaderLayerInterfaceVersion"
            },
            "enable_environment": {
                "SNOWGLASS_VULKAN_LAYER": "1"
            },
            "disable_environment": {
                "DISABLE_SNOWGLASS_VULKAN_LAYER": "1"
            }
        }
    });
    let file = std::fs::File::create(directory.path().join("snowglass-layer.json"))?;
    serde_json::to_writer_pretty(file, &manifest).map_err(std::io::Error::other)?;
    Ok(directory)
}

fn hook_libraries(
    hooks: &[HookBackend],
    input_hooks: &[crate::InputHookBackend],
    isolate: bool,
) -> Vec<InjectionLibrary> {
    let mut libraries = Vec::new();
    if hooks
        .iter()
        .any(|hook| matches!(hook, HookBackend::D3d11 | HookBackend::D3d12))
    {
        libraries.push(InjectionLibrary::new(
            "DXGI",
            "snowglass_hook_dxgi.dll",
            c"SnowglassInstallDxgiHook",
        ));
    }
    if hooks.contains(&HookBackend::D3d11) {
        libraries.push(InjectionLibrary::new(
            "D3D11",
            "snowglass_hook_d3d11.dll",
            c"SnowglassInstallD3d11Hook",
        ));
    }
    if hooks.contains(&HookBackend::D3d12) {
        libraries.push(InjectionLibrary::new(
            "D3D12",
            "snowglass_hook_d3d12.dll",
            c"SnowglassInstallD3d12Hook",
        ));
    }
    if hooks.contains(&HookBackend::OpenGl) {
        libraries.push(InjectionLibrary::new(
            "OpenGL",
            "snowglass_hook_opengl32.dll",
            c"SnowglassInstallOpenGlHook",
        ));
    }
    if hooks.contains(&HookBackend::Gdi) {
        libraries.push(InjectionLibrary::new(
            "GDI",
            "snowglass_hook_gdi.dll",
            c"SnowglassInstallGdiHook",
        ));
    }
    libraries.push(InjectionLibrary::new(
        "DirectInput",
        "snowglass_hook_dinput.dll",
        c"SnowglassInstallDinputHook",
    ));
    libraries.push(InjectionLibrary::new(
        "Win32 input",
        "snowglass_hook_win32.dll",
        c"SnowglassInstallWin32Hook",
    ));
    if input_hooks.contains(&crate::InputHookBackend::BluetoothApiWin32) {
        libraries.push(InjectionLibrary::new(
            "Bluetooth HID API",
            "snowglass_hook_bluetoothapi_win32.dll",
            c"SnowglassInstallBluetoothApiWin32Hook",
        ));
    }
    if isolate {
        libraries.push(InjectionLibrary::new(
            "UI",
            "snowglass_hook_ui.dll",
            c"SnowglassInstallUiHook",
        ));
    }
    libraries.push(InjectionLibrary::new(
        "SDL2",
        "snowglass_hook_sdl2.dll",
        c"SnowglassInstallSdl2Hook",
    ));
    libraries.push(InjectionLibrary::new(
        "SDL3",
        "snowglass_hook_sdl3.dll",
        c"SnowglassInstallSdl3Hook",
    ));
    libraries
}
