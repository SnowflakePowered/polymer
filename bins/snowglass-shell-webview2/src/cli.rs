use clap::{Arg, ArgAction};
use snowglass_pluginhost::{EmulatorPluginInstance, PluginHost};
use snowglass_renderer::cli::{
    ViewerOptions, viewer_command_named, viewer_options_from_matches_with_default_machine,
};
use std::{error::Error, fs, path::PathBuf};

#[derive(Debug)]
pub struct GameLaunchRequest {
    pub game: PathBuf,
    pub emulator: Option<String>,
}

pub struct ShellOptions {
    pub viewer: ViewerOptions,
    pub foreground_url: String,
    pub plugins: PluginHost,
    pub game_launch: Option<GameLaunchRequest>,
}

pub fn parse() -> Result<ShellOptions, Box<dyn Error>> {
    let matches = viewer_command_named("snowglass-shell-webview2")
        .arg(
            Arg::new("url")
                .long("url")
                .required(true)
                .value_name("URL")
                .help("Foreground React application URL"),
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
    let plugins = PluginHost::new();
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
        foreground_url: matches
            .get_one::<String>("url")
            .expect("required URL is present")
            .to_owned(),
        plugins,
        game_launch,
    })
}
