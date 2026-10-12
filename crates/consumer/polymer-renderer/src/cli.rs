//! Shared command-line contract for Polymer renderer hosts.

use clap::ValueEnum;
use clap::{ArgAction, ArgMatches, Command, CommandFactory, FromArgMatches, Parser};
pub use polymer_runtime::GraphicsHook;
use std::{collections::HashSet, error::Error};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum ViewerApi {
    #[cfg_attr(windows, default)]
    D3d12,
    Vulkan,
    #[cfg_attr(not(windows), default)]
    Metal,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum HookChoice {
    Auto,
    Vulkan,
    #[value(name = "opengl")]
    OpenGl,
    D3d12,
    D3d11,
    Metal,
}

impl HookChoice {
    fn backend(self) -> Option<GraphicsHook> {
        match self {
            Self::Auto => None,
            Self::Vulkan => Some(GraphicsHook::Vulkan),
            Self::OpenGl => Some(GraphicsHook::OpenGl),
            Self::D3d12 => Some(GraphicsHook::D3d12),
            Self::D3d11 => Some(GraphicsHook::D3d11),
            Self::Metal => Some(GraphicsHook::Metal),
        }
    }
}

/// The `--hook auto` selection, ordered from most to least likely to be the
/// application's real renderer.
#[cfg(windows)]
pub const AUTO_HOOKS: [GraphicsHook; 3] = [
    GraphicsHook::Vulkan,
    GraphicsHook::D3d12,
    GraphicsHook::D3d11,
];

#[cfg(target_os = "macos")]
pub const AUTO_HOOKS: [GraphicsHook; 1] = [GraphicsHook::Metal];

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
    pub layout_css: Option<String>,
    pub surfaces: u32,
    pub skip_surfaces: u32,
    pub isolate: bool,
    pub api: ViewerApi,
    pub hooks: Vec<GraphicsHook>,
}

#[derive(Parser)]
#[command(name = "polymer-viewer", version, about)]
struct ViewerArguments {
    /// Graphics API used to render the viewer window.
    #[arg(long, value_enum, default_value_t)]
    pub(crate) api: ViewerApi,

    /// Select configured swapchain surface indices.
    /// Comma-separated entries may append `:TURNS:WINDOW_REGEX` to specify
    /// counterclockwise quarter turns and a presenting-window title pattern.
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

    /// CSS declarations applied to the Taffy container that holds render
    /// targets. Without this option the viewer uses its original vertical
    /// layout. Example: `display: flex; flex-direction: row; gap: 12px`.
    #[arg(long, value_name = "CSS")]
    pub(crate) layout_css: Option<String>,
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
    fn validate_presentation(&self) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    fn into_options(self) -> Result<ViewerOptions, Box<dyn Error>> {
        let mut hooks = Vec::new();
        expand_hook_choices(&mut hooks, self.hook_choices.iter().copied())?;
        if hooks.is_empty() {
            hooks.extend(AUTO_HOOKS);
        }
        self.validate_presentation()?;
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
            layout_css: self.layout_css,
            surfaces: self.surfaces,
            skip_surfaces: self.skip_surfaces,
            isolate: !self.no_isolate,
            api: self.api,
            hooks,
        })
    }
}

pub fn parse_target() -> Result<ViewerOptions, Box<dyn Error>> {
    parse_target_named("polymer-viewer")
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
    ViewerArguments::try_parse_from(std::iter::once("polymer-viewer".to_owned()).chain(arguments))?
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
