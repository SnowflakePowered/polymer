use clap::Arg;
use snowglass_renderer::cli::{ViewerOptions, viewer_command_named, viewer_options_from_matches};
use std::error::Error;

pub struct ShellOptions {
    pub viewer: ViewerOptions,
    pub foreground_url: String,
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
        .get_matches();
    Ok(ShellOptions {
        viewer: viewer_options_from_matches(&matches)?,
        foreground_url: matches
            .get_one::<String>("url")
            .expect("required URL is present")
            .to_owned(),
    })
}
