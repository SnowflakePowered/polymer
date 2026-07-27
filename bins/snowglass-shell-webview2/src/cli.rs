use clap::Arg;
use snowglass_renderer::cli::{ViewerOptions, viewer_command_named, viewer_options_from_matches};
use std::{error::Error, path::PathBuf};

pub struct ShellOptions {
    pub viewer: ViewerOptions,
    pub foreground_url: String,
    /// HTML file laying out the background and the game views. When given, the
    /// shell lays that layer out itself and composites every stream into one
    /// swapchain with it.
    pub game_layer: Option<PathBuf>,
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
            Arg::new("game-layer")
                .long("game-layer")
                .value_name("HTML")
                .value_parser(clap::value_parser!(PathBuf))
                .help("Override the built-in HTML that lays out the background and game views"),
        )
        .get_matches();
    Ok(ShellOptions {
        viewer: viewer_options_from_matches(&matches)?,
        foreground_url: matches
            .get_one::<String>("url")
            .expect("required URL is present")
            .to_owned(),
        game_layer: matches.get_one::<PathBuf>("game-layer").cloned(),
    })
}
