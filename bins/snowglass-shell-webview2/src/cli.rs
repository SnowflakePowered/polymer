use clap::Arg;
use snowglass_renderer::cli::{ViewerOptions, viewer_command_named, viewer_options_from_matches};
use std::error::Error;
use url::Url;

pub struct ShellOptions {
    pub viewer: ViewerOptions,
    pub foreground_url: String,
    pub background_url: String,
}

pub fn parse() -> Result<ShellOptions, Box<dyn Error>> {
    let matches = viewer_command_named("snowglass-shell-webview2")
        .arg(
            Arg::new("url")
                .long("url")
                .required(true)
                .value_name("URL")
                .help("Foreground React application URL; the background loads sibling back.html"),
        )
        .get_matches();
    let foreground_url = matches
        .get_one::<String>("url")
        .expect("required URL is present")
        .to_owned();
    let background_url = sibling_background_url(&foreground_url)?;
    Ok(ShellOptions {
        viewer: viewer_options_from_matches(&matches)?,
        foreground_url,
        background_url,
    })
}

fn sibling_background_url(foreground: &str) -> Result<String, Box<dyn Error>> {
    let mut url = Url::parse(foreground)
        .map_err(|error| format!("invalid frontend URL '{foreground}': {error}"))?;
    if url.cannot_be_a_base() {
        return Err(format!("frontend URL cannot have a sibling back.html: '{foreground}'").into());
    }
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|()| format!("frontend URL cannot have a path: '{foreground}'"))?;
        segments.pop();
        segments.push("back.html");
    }
    url.set_fragment(None);
    Ok(url.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_the_background_as_a_sibling_page() {
        assert_eq!(
            sibling_background_url("http://localhost:5173/ui/shell.html?theme=dark#menu").unwrap(),
            "http://localhost:5173/ui/back.html?theme=dark"
        );
        assert_eq!(
            sibling_background_url("http://localhost:5173/ui/").unwrap(),
            "http://localhost:5173/ui/back.html"
        );
    }
}
