//! Shared command-line contract for Snowglass renderer hosts.

use clap::ValueEnum;
use clap::{ArgAction, ArgMatches, Command, CommandFactory, FromArgMatches, Parser};
pub use snowglass_runtime::GraphicsHook;
use std::{collections::HashSet, error::Error, net::SocketAddr};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum ViewerApi {
    #[default]
    D3d12,
    Vulkan,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum HookChoice {
    Auto,
    Vulkan,
    #[value(name = "opengl")]
    OpenGl,
    D3d12,
    D3d11,
    Gdi,
}

impl HookChoice {
    fn backend(self) -> Option<GraphicsHook> {
        match self {
            Self::Auto => None,
            Self::Vulkan => Some(GraphicsHook::Vulkan),
            Self::OpenGl => Some(GraphicsHook::OpenGl),
            Self::D3d12 => Some(GraphicsHook::D3d12),
            Self::D3d11 => Some(GraphicsHook::D3d11),
            Self::Gdi => Some(GraphicsHook::Gdi),
        }
    }
}

/// The `--hook auto` selection, ordered from most to least likely to be the
/// application's real renderer.
pub const AUTO_HOOKS: [GraphicsHook; 4] = [
    GraphicsHook::Vulkan,
    GraphicsHook::OpenGl,
    GraphicsHook::D3d12,
    GraphicsHook::D3d11,
];

fn expand_hook_choices(
    hooks: &mut Vec<GraphicsHook>,
    choices: impl IntoIterator<Item = HookChoice>,
) -> Result<(), Box<dyn Error>> {
    for choice in choices {
        if matches!(choice, HookChoice::Auto) {
            for &hook in &AUTO_HOOKS {
                if !hooks.contains(&hook) {
                    hooks.push(hook);
                }
            }
            continue;
        }
        let hook = choice
            .backend()
            .expect("non-auto hook choice has a backend");
        if hooks.contains(&hook) {
            return Err(format!("hook '{choice:?}' is listed more than once").into());
        }
        hooks.push(hook);
    }
    Ok(())
}

/// One `--track-render-targets` entry: an index into the detected selection,
/// viewer rotation, and an optional presenting-window title regex.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrackedRenderTarget {
    pub index: u32,
    pub quarter_turns: u8,
    pub surface_pattern: Option<String>,
}

#[derive(Clone)]
pub struct ViewerOptions {
    pub target: Vec<String>,
    pub track_render_targets: Option<Vec<TrackedRenderTarget>>,
    pub graph_tracking: bool,
    pub layout_css: Option<String>,
    pub surfaces: u32,
    pub skip_surfaces: u32,
    pub isolate: bool,
    pub resize: bool,
    pub api: ViewerApi,
    pub force_keyed_mutex: bool,
    pub hooks: Vec<GraphicsHook>,
    pub webrtc: Option<Vec<u32>>,
    pub bind_webrtc: SocketAddr,
}

#[derive(Parser)]
#[command(name = "snowglass-viewer", version, about)]
struct ViewerArguments {
    /// Graphics API used to render the viewer window.
    #[arg(long, value_enum, default_value_t)]
    pub(crate) api: ViewerApi,

    /// Track render targets by index into the detected selection, as a
    /// comma-separated list. An entry may append `:TURNS` to rotate that
    /// target counterclockwise by quarter turns, then `:WINDOW_REGEX` to
    /// resolve the target within a presenting window whose title matches
    /// (`0:0:GamePad View,1` pins target 0 to the GamePad window).
    #[arg(
        long,
        visible_alias = "track-render-target",
        num_args = 0..=1,
        default_missing_value = "0",
        value_delimiter = ',',
        value_name = "INDEXES",
        value_parser = parse_render_target_index
    )]
    pub(crate) track_render_targets: Option<Vec<TrackedRenderTarget>>,

    /// Resolve tracked render targets through the presentation dependency
    /// graph (offscreen screens behind compositor blits and copies) instead
    /// of the scissor and one-hop heuristics.
    #[arg(long, action = ArgAction::SetTrue, requires = "track_render_targets")]
    pub(crate) graph_tracking: bool,

    /// CSS declarations applied to the Taffy container that holds render
    /// targets. Without this option the viewer uses its original vertical
    /// layout. Example: `display: flex; flex-direction: row; gap: 12px`.
    #[arg(long, value_name = "CSS")]
    pub(crate) layout_css: Option<String>,

    /// Publish selected render-target indices as tracks on one WebRTC stream.
    #[arg(
        long,
        num_args = 0..,
        default_missing_value = "0",
        value_delimiter = ',',
        value_name = "INDEXES",
        value_parser = parse_webrtc_target
    )]
    pub(crate) webrtc: Option<Vec<u32>>,

    /// Address for the WebRTC HTTP signaling server.
    #[arg(
        long,
        default_value = "127.0.0.1:8080",
        value_name = "IP:PORT",
        requires = "webrtc"
    )]
    pub(crate) bind_webrtc: SocketAddr,

    /// Maximum number of capture surfaces to display.
    #[arg(
        long,
        default_value_t = 16,
        num_args = 0..=1,
        default_missing_value = "16",
        value_parser = parse_surface_count
    )]
    pub(crate) surfaces: u32,

    /// Number of matching capture surfaces to skip.
    #[arg(
        long,
        visible_aliases = ["skip-surface", "skip-window", "skip-windows"],
        default_value_t = 0,
        num_args = 0..=1,
        default_missing_value = "1",
        value_parser = parse_skip_count
    )]
    pub(crate) skip_surfaces: u32,

    /// Explicitly enable target process isolation (the default).
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "no_isolate")]
    pub(crate) isolate: bool,

    /// Run the target on the current desktop without process isolation.
    #[arg(long, action = ArgAction::SetTrue)]
    no_isolate: bool,

    /// Resize guest windows to their displayed extent. Only applies in isolated mode.
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "no_isolate")]
    pub(crate) resize: bool,

    /// Force keyed-mutex synchronization for compatible capture backends.
    #[arg(long = "force-km")]
    pub(crate) force_keyed_mutex: bool,

    /// Capture hooks to install, as an ordered comma-separated list.
    #[arg(
        long = "hook",
        action = ArgAction::Append,
        value_enum,
        value_delimiter = ',',
        value_name = "HOOKS"
    )]
    hook_choices: Vec<HookChoice>,

    /// Target program and its arguments. Must follow `--`.
    #[arg(last = true, num_args = 0.., value_name = "COMMAND")]
    pub(crate) target: Vec<String>,
}

impl ViewerArguments {
    fn into_options(self) -> Result<ViewerOptions, Box<dyn Error>> {
        let mut hooks = Vec::new();
        expand_hook_choices(&mut hooks, self.hook_choices)?;
        if hooks.is_empty() {
            hooks.extend(AUTO_HOOKS);
        }
        if let Some(targets) = &self.webrtc {
            let mut unique = HashSet::new();
            if targets.iter().any(|target| !unique.insert(*target)) {
                return Err("WebRTC render-target indices must be unique".into());
            }
        }
        if let Some(targets) = &self.track_render_targets {
            let mut unique = HashSet::new();
            if targets.iter().any(|target| !unique.insert(target.index)) {
                return Err("render-target indices must be unique".into());
            }
            for target in targets {
                if let Some(pattern) = &target.surface_pattern {
                    regex::Regex::new(pattern).map_err(|error| {
                        format!("invalid render-target surface regex '{pattern}': {error}")
                    })?;
                }
            }
        }
        Ok(ViewerOptions {
            target: self.target,
            track_render_targets: self.track_render_targets,
            graph_tracking: self.graph_tracking,
            layout_css: self.layout_css,
            surfaces: self.surfaces,
            skip_surfaces: self.skip_surfaces,
            isolate: !self.no_isolate,
            resize: self.resize,
            api: self.api,
            force_keyed_mutex: self.force_keyed_mutex,
            hooks,
            webrtc: self.webrtc,
            bind_webrtc: self.bind_webrtc,
        })
    }
}

pub fn parse_target() -> Result<ViewerOptions, Box<dyn Error>> {
    parse_target_named("snowglass-viewer")
}

/// Parses the current process arguments while allowing a renderer host to
/// supply its own executable name in help and error output.
pub fn parse_target_named(name: &'static str) -> Result<ViewerOptions, Box<dyn Error>> {
    let matches = viewer_command_named(name).get_matches();
    viewer_options_from_matches(&matches)
}

/// Builds the shared viewer command so a renderer host can append
/// host-specific arguments without duplicating the renderer CLI.
pub fn viewer_command_named(name: &'static str) -> Command {
    ViewerArguments::command().name(name)
}

/// Decodes shared viewer options from matches produced by
/// [`viewer_command_named`], ignoring arguments added by a renderer host.
pub fn viewer_options_from_matches(matches: &ArgMatches) -> Result<ViewerOptions, Box<dyn Error>> {
    ViewerArguments::from_arg_matches(matches)?.into_options()
}

pub fn split_command_arguments(value: &str) -> Result<Vec<String>, String> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for character in value.chars() {
        if character == '"' {
            quoted = !quoted;
        } else if character.is_whitespace() && !quoted {
            if !current.is_empty() {
                arguments.push(std::mem::take(&mut current));
            }
        } else {
            current.push(character);
        }
    }
    if quoted {
        return Err("Arguments contain an unmatched quote".to_owned());
    }
    if !current.is_empty() {
        arguments.push(current);
    }
    Ok(arguments)
}

/// Parses an explicit argument sequence. Renderer hosts can use this when
/// arguments come from an embedding API rather than the current process.
pub fn parse_arguments(
    arguments: impl IntoIterator<Item = String>,
) -> Result<ViewerOptions, Box<dyn Error>> {
    ViewerArguments::try_parse_from(
        std::iter::once("snowglass-viewer".to_owned()).chain(arguments),
    )?
    .into_options()
}

fn parse_render_target_index(value: &str) -> Result<TrackedRenderTarget, String> {
    let mut fields = value.splitn(3, ':');
    let index = fields.next().unwrap_or_default();
    let turns = fields.next();
    let surface_pattern = fields.next();
    let index = index
        .parse::<u32>()
        .map_err(|_| format!("invalid render-target index '{value}'; expected 0..=15"))?;
    if index >= 16 {
        return Err(format!(
            "invalid render-target index '{value}'; expected 0..=15"
        ));
    }
    let quarter_turns = match turns {
        Some("") => {
            return Err(format!(
                "invalid rotation in '{value}'; expected counterclockwise quarter turns"
            ));
        }
        Some(turns) => {
            (turns.parse::<u32>().map_err(|_| {
                format!("invalid rotation '{turns}'; expected counterclockwise quarter turns")
            })? % 4) as u8
        }
        None => 0,
    };
    Ok(TrackedRenderTarget {
        index,
        quarter_turns,
        surface_pattern: surface_pattern
            .filter(|pattern| !pattern.is_empty())
            .map(str::to_owned),
    })
}

fn parse_webrtc_target(value: &str) -> Result<u32, String> {
    let index = value
        .parse::<u32>()
        .map_err(|_| format!("invalid WebRTC render-target index '{value}'"))?;
    if index >= 16 {
        return Err(format!(
            "invalid WebRTC render-target index '{value}'; expected 0..=15"
        ));
    }
    Ok(index)
}

fn parse_surface_count(value: &str) -> Result<u32, String> {
    parse_bounded_count(value, "surface")
}

fn parse_bounded_count(value: &str, name: &str) -> Result<u32, String> {
    let count = value
        .parse::<u32>()
        .map_err(|_| format!("invalid {name} count '{value}'; expected 1..=16"))?;
    if !(1..=16).contains(&count) {
        return Err(format!("invalid {name} count '{value}'; expected 1..=16"));
    }
    Ok(count)
}

fn parse_skip_count(value: &str) -> Result<u32, String> {
    value.parse::<u32>().map_err(|_| {
        format!("invalid surface skip count '{value}'; expected a non-negative integer")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_shared_by_renderer_hosts() {
        let options = parse_arguments(["--".to_owned(), "game.exe".to_owned()]).unwrap();
        assert_eq!(options.api, ViewerApi::D3d12);
        assert_eq!(options.hooks, AUTO_HOOKS);
        assert_eq!(options.target, ["game.exe"]);
        assert_eq!(options.webrtc, None);
    }

    #[test]
    fn validates_deduplicated_render_and_video_targets() {
        assert!(
            parse_arguments([
                "--track-render-targets=0,0".to_owned(),
                "--".to_owned(),
                "game.exe".to_owned(),
            ])
            .is_err()
        );
        assert!(
            parse_arguments([
                "--webrtc=0,0".to_owned(),
                "--".to_owned(),
                "game.exe".to_owned(),
            ])
            .is_err()
        );
    }
}
