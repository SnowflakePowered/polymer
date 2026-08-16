use crate::{
    input::GamepadMapping,
    launch::{GraphicsHook, UnattachedLaunchPlan, UnattachedTrackedSurface},
};
use clap::{Arg, ArgAction};
use snowglass_pluginhost::{EmulatorPluginInstance, PluginRegistry};
use snowglass_renderer::cli::{
    HookBackend, ViewerOptions, viewer_command_named,
    viewer_options_from_matches_with_default_machine,
};
use std::{error::Error, fs, path::PathBuf};

#[derive(Debug)]
pub struct GameLaunchRequest {
    pub game: PathBuf,
    pub emulator: Option<String>,
}

pub struct ShellOptions {
    pub viewer: ViewerOptions,
    pub ui_url: String,
    pub plugins: PluginRegistry,
    pub game_launch: Option<GameLaunchRequest>,
}

pub fn launch_from_viewer(options: &ViewerOptions) -> Result<Option<UnattachedLaunchPlan>, String> {
    let Some(command) = options.target.first() else {
        return Ok(None);
    };
    let hook = options
        .hooks
        .iter()
        .map(|hook| match hook {
            HookBackend::OpenGl => Ok(GraphicsHook::Opengl),
            HookBackend::Vulkan => Ok(GraphicsHook::Vulkan),
            HookBackend::D3d11 => Ok(GraphicsHook::D3d11),
            HookBackend::D3d12 => Ok(GraphicsHook::D3d12),
            HookBackend::Gdi => Err("the shell emulator API does not expose GDI".to_owned()),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let tracked = options.track_render_targets.clone().unwrap_or_else(|| {
        vec![snowglass_renderer::cli::TrackedRenderTarget {
            index: 0,
            quarter_turns: 0,
            surface_pattern: None,
        }]
    });
    let webrtc = options.webrtc.as_deref().unwrap_or_default();
    let surfaces = tracked
        .into_iter()
        .map(|surface| UnattachedTrackedSurface {
            index: surface.index,
            rotation: i32::from(surface.quarter_turns),
            selector: surface.surface_pattern,
            webrtc: webrtc.contains(&surface.index),
        })
        .collect();
    let launch = UnattachedLaunchPlan {
        command: command.clone(),
        arguments: options.target[1..].to_vec(),
        isolated: options.isolate,
        hook,
        surfaces,
        gamepad_mapping: options.gamepads.iter().map(GamepadMapping::from).collect(),
        machine: options.machine.clone(),
        webrtc_bind: options
            .webrtc
            .as_ref()
            .map(|_| options.bind_webrtc.to_string()),
        generated_configuration: None,
        prepared_filesystem: None,
    };
    launch.validate()?;
    Ok(Some(launch))
}

pub fn parse() -> Result<ShellOptions, Box<dyn Error>> {
    let matches = viewer_command_named("snowglass-shell-webview2")
        .arg(
            Arg::new("url")
                .long("url")
                .required(true)
                .value_name("URL")
                .help("URL of the single Solid application document"),
        )
        .arg(
            Arg::new("emulator")
                .long("emulator")
                .value_name("ID")
                .requires("game")
                .conflicts_with("target")
                .help("Emulator plugin package ID"),
        )
        .arg(
            Arg::new("game")
                .long("game")
                .value_name("PATH")
                .conflicts_with("target")
                .action(ArgAction::Set)
                .help("Game package to detect and launch with a compatible emulator plugin"),
        )
        .get_matches();
    let plugins = PluginRegistry::new();
    let plugin_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("plugins");
    if plugin_root.is_dir() {
        let mut manifests = fs::read_dir(&plugin_root)?
            .filter_map(Result::ok)
            .map(|entry| entry.path().join("plugin.toml"))
            .filter(|manifest| manifest.is_file())
            .collect::<Vec<_>>();
        manifests.sort();
        for manifest in manifests {
            if let Err(error) = plugins.load(&manifest) {
                eprintln!(
                    "snowglass: skipping emulator plugin {}: {error}",
                    manifest.display()
                );
            }
        }
    }
    let emulator = matches
        .get_one::<String>("emulator")
        .map(|emulator| -> Result<_, Box<dyn Error>> {
            if emulator.is_empty()
                || !emulator
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            {
                return Err(format!("invalid emulator plugin ID '{emulator}'").into());
            }
            plugins
                .plugin::<dyn EmulatorPluginInstance>(emulator)
                .ok_or_else(|| format!("plugin '{emulator}' is not a loaded emulator plugin"))?;
            Ok(emulator.clone())
        })
        .transpose()?;
    let game_launch = matches
        .get_one::<String>("game")
        .map(|game| GameLaunchRequest {
            game: PathBuf::from(game),
            emulator: emulator.clone(),
        });
    let default_machine = emulator
        .as_ref()
        .and_then(|emulator| {
            let plugin = plugins.plugin::<dyn EmulatorPluginInstance>(emulator)?;
            let [platform] = plugin.platforms() else {
                return None;
            };
            Some(platform.clone())
        })
        .or_else(|| game_launch.as_ref().map(|_| String::new()));
    Ok(ShellOptions {
        viewer: viewer_options_from_matches_with_default_machine(&matches, default_machine)?,
        ui_url: matches
            .get_one::<String>("url")
            .expect("required URL is present")
            .to_owned(),
        plugins,
        game_launch,
    })
}
