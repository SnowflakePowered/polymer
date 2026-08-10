use clap::{Arg, ArgAction};
use snowglass_pluginhost::{EmulatorPluginInstance, PluginHost};
use snowglass_renderer::cli::{
    ViewerOptions, viewer_command_named, viewer_options_from_matches_with_default_machine,
};
use std::{error::Error, path::PathBuf, sync::Arc};

pub struct PluginLaunchRequest {
    pub plugin: Arc<dyn EmulatorPluginInstance>,
    pub game: PathBuf,
}

pub struct ShellOptions {
    pub viewer: ViewerOptions,
    pub foreground_url: String,
    pub plugins: PluginHost,
    pub plugin_launch: Option<PluginLaunchRequest>,
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
                .requires("emulator")
                .action(ArgAction::Set)
                .help("Game path passed to the emulator plugin"),
        )
        .get_matches();
    let plugins = PluginHost::new();
    let plugin_launch = matches
        .get_one::<String>("emulator")
        .map(|emulator| -> Result<_, Box<dyn Error>> {
            if emulator.is_empty()
                || !emulator
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            {
                return Err(format!("invalid emulator plugin ID '{emulator}'").into());
            }
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("plugins")
                .join(emulator);
            plugins.load(root.join("plugin.toml"))?;
            let plugin = plugins
                .plugin::<dyn EmulatorPluginInstance>(emulator)
                .ok_or_else(|| format!("plugin '{emulator}' has no emulator capability"))?;
            let game = PathBuf::from(
                matches
                    .get_one::<String>("game")
                    .expect("--emulator requires --game"),
            )
            .canonicalize()?;
            Ok(PluginLaunchRequest { plugin, game })
        })
        .transpose()?;
    let default_machine = plugin_launch.as_ref().and_then(|request| {
        let [platform] = request.plugin.platforms() else {
            return None;
        };
        Some(platform.clone())
    });
    Ok(ShellOptions {
        viewer: viewer_options_from_matches_with_default_machine(&matches, default_machine)?,
        foreground_url: matches
            .get_one::<String>("url")
            .expect("required URL is present")
            .to_owned(),
        plugins,
        plugin_launch,
    })
}
