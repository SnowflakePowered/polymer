use super::*;
use clap::{ArgAction, Parser};

/// One `--track-render-targets` entry: an index into the detected selection
/// plus the number of counterclockwise quarter turns applied to that target
/// when compositing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TrackedRenderTarget {
    pub(crate) index: u32,
    pub(crate) quarter_turns: u8,
}

pub(crate) struct ViewerOptions {
    pub(crate) target: Vec<String>,
    pub(crate) track_render_targets: Option<Vec<TrackedRenderTarget>>,
    pub(crate) graph_tracking: bool,
    pub(crate) layout_css: Option<String>,
    pub(crate) surfaces: u32,
    pub(crate) skip_surfaces: u32,
    pub(crate) isolate: bool,
    pub(crate) api: ViewerApi,
    pub(crate) force_keyed_mutex: bool,
    pub(crate) hooks: Vec<HookBackend>,
    pub(crate) webrtc: Option<Vec<u32>>,
    pub(crate) bind_webrtc: SocketAddr,
    pub(crate) gamepads: Vec<GamepadArgument>,
    pub(crate) stone_db: Option<PathBuf>,
    pub(crate) gamecontroller_db: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GamepadArgument {
    pub(crate) selector: DeviceSelector,
    pub(crate) slot: usize,
    pub(crate) layout: String,
    pub(crate) force_mouse: bool,
}

#[derive(Parser)]
#[command(name = "snowglass-viewer", version, about)]
struct ViewerArguments {
    /// Graphics API used to render the viewer window.
    #[arg(long, value_enum, default_value_t)]
    pub(crate) api: ViewerApi,

    /// Map an SDL device index, exact name, GUID, or K (keyboard) to a virtual slot and Stone layout.
    /// Append +M to the selector to force mouse pointer axes instead of its touchpad.
    #[arg(long = "gamepad", action = ArgAction::Append, value_name = "SELECTOR:SLOT:LAYOUT")]
    pub(crate) gamepads: Vec<GamepadArgument>,

    /// Updated Stone JSON database. The embedded database is used by default.
    #[arg(long, value_name = "FILE")]
    pub(crate) stone_db: Option<PathBuf>,

    /// Updated SDL gamecontrollerdb.txt. The embedded database is used by default.
    #[arg(long, value_name = "FILE")]
    pub(crate) gamecontroller_db: Option<PathBuf>,

    /// Track render targets by index into the detected selection, as a
    /// comma-separated list. An entry may append `:TURNS` to rotate that
    /// target counterclockwise by quarter turns when compositing
    /// (`0:1,1` shows target 0 rotated 90 degrees above target 1).
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
        }
        let mut gamepads = self.gamepads;
        if gamepads.is_empty() {
            gamepads.push("0:0:XBOX_CONTROLLER".parse()?);
        }
        gamepads.sort_by_key(|gamepad| gamepad.slot);
        for (expected, gamepad) in gamepads.iter().enumerate() {
            if gamepad.slot != expected || expected > u8::MAX as usize {
                return Err("gamepad slots must be unique and contiguous from zero".into());
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
            api: self.api,
            force_keyed_mutex: self.force_keyed_mutex,
            hooks,
            webrtc: self.webrtc,
            bind_webrtc: self.bind_webrtc,
            gamepads,
            stone_db: self.stone_db,
            gamecontroller_db: self.gamecontroller_db,
        })
    }
}

pub(crate) fn parse_target() -> Result<ViewerOptions, Box<dyn Error>> {
    ViewerArguments::parse().into_options()
}

pub(crate) fn split_command_arguments(value: &str) -> Result<Vec<String>, String> {
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

#[cfg(test)]
fn parse_arguments(
    arguments: impl IntoIterator<Item = String>,
) -> Result<ViewerOptions, Box<dyn Error>> {
    ViewerArguments::try_parse_from(
        std::iter::once("snowglass-viewer".to_owned()).chain(arguments),
    )?
    .into_options()
}

fn parse_render_target_index(value: &str) -> Result<TrackedRenderTarget, String> {
    let (index, turns) = match value.split_once(':') {
        Some((index, turns)) => (index, Some(turns)),
        None => (value, None),
    };
    let index = index
        .parse::<u32>()
        .map_err(|_| format!("invalid render-target index '{value}'; expected 0..=15"))?;
    if index >= 16 {
        return Err(format!(
            "invalid render-target index '{value}'; expected 0..=15"
        ));
    }
    let quarter_turns = match turns {
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

impl std::str::FromStr for GamepadArgument {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut fields = value.rsplitn(3, ':');
        let layout = fields.next().filter(|field| !field.is_empty());
        let slot = fields.next();
        let selector = fields.next().filter(|field| !field.is_empty());
        let (Some(layout), Some(slot), Some(selector)) = (layout, slot, selector) else {
            return Err(format!(
                "invalid gamepad mapping '{value}'; expected SELECTOR:SLOT:LAYOUT"
            ));
        };
        let slot = slot
            .parse::<usize>()
            .map_err(|_| format!("invalid virtual slot '{slot}' in gamepad mapping '{value}'"))?;
        if slot > u8::MAX as usize {
            return Err(format!("virtual gamepad slot {slot} is outside 0..=255"));
        }
        let (selector, force_mouse) = selector
            .strip_suffix("+M")
            .map_or((selector, false), |selector| (selector, true));
        if selector.is_empty() {
            return Err(format!(
                "missing selector before +M in gamepad mapping '{value}'"
            ));
        }
        let compact_guid = selector.replace('-', "");
        let selector = if selector == "K" {
            DeviceSelector::Keyboard
        } else if let Ok(index) = selector.parse() {
            DeviceSelector::Index(index)
        } else if compact_guid.len() == 32
            && compact_guid.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            DeviceSelector::Guid(compact_guid.to_ascii_lowercase())
        } else {
            DeviceSelector::Name(selector.to_owned())
        };
        let force_mouse = force_mouse || matches!(selector, DeviceSelector::Keyboard);
        Ok(Self {
            selector,
            slot,
            layout: layout.to_owned(),
            force_mouse,
        })
    }
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
mod argument_tests {
    use super::*;

    fn arguments(values: &[&str]) -> Result<ViewerOptions, Box<dyn Error>> {
        parse_arguments(values.iter().map(|value| (*value).to_owned()))
    }

    fn tracked(index: u32, quarter_turns: u8) -> TrackedRenderTarget {
        TrackedRenderTarget {
            index,
            quarter_turns,
        }
    }

    #[test]
    fn defaults_to_d3d12() -> Result<(), Box<dyn Error>> {
        let options = arguments(&["--", "game.exe"])?;
        assert_eq!(options.api, ViewerApi::D3d12);
        assert_eq!(options.track_render_targets, None);
        assert_eq!(options.layout_css, None);
        assert_eq!(options.surfaces, 16);
        assert_eq!(options.skip_surfaces, 0);
        assert!(options.isolate);
        assert!(!options.force_keyed_mutex);
        assert_eq!(options.hooks, AUTO_HOOKS);
        assert_eq!(options.target, ["game.exe"]);
        assert_eq!(options.webrtc, None);
        assert_eq!(options.bind_webrtc, "127.0.0.1:8080".parse()?);
        assert_eq!(
            options.gamepads,
            [GamepadArgument {
                selector: DeviceSelector::Index(0),
                slot: 0,
                layout: "XBOX_CONTROLLER".to_owned(),
                force_mouse: false,
            }]
        );
        assert_eq!(options.stone_db, None);
        assert_eq!(options.gamecontroller_db, None);
        Ok(())
    }

    #[test]
    fn accepts_render_target_layout_css() -> Result<(), Box<dyn Error>> {
        let options = arguments(&[
            "--layout-css",
            "display: grid; grid-template-columns: 1fr 2fr",
            "--",
            "game.exe",
        ])?;
        assert_eq!(
            options.layout_css.as_deref(),
            Some("display: grid; grid-template-columns: 1fr 2fr")
        );
        Ok(())
    }

    #[test]
    fn cli_target_arguments_are_not_reparsed_or_requoted() -> Result<(), Box<dyn Error>> {
        let options = arguments(&[
            "--track-render-targets",
            "0",
            "--hook",
            "d3d11",
            "--no-isolate",
            "--gamepad",
            "0:0:WII_REMOTE_CONTROLLER",
            "--",
            r#"..\Dolphin-ARM64\Dolphin.exe"#,
            "-v",
            "D3D11",
            r#"..\Wii Sports (USA) (Rev 1).rvz"#,
        ])?;
        let expected = options.target.clone();
        assert_eq!(LauncherState::from_options(&options).target()?, expected);
        assert_eq!(expected[3], r#"..\Wii Sports (USA) (Rev 1).rvz"#);
        Ok(())
    }

    #[test]
    fn accepts_no_target_for_interactive_launcher() -> Result<(), Box<dyn Error>> {
        let options = arguments(&[])?;
        assert!(options.target.is_empty());
        assert_eq!(options.api, ViewerApi::D3d12);
        assert_eq!(options.hooks, AUTO_HOOKS);
        assert_eq!(options.gamepads.len(), 1);
        Ok(())
    }

    #[test]
    fn launcher_splits_quoted_windows_arguments() {
        assert_eq!(
            split_command_arguments(r#"--profile "C:\Users\Player One\game.ini" --fullscreen"#)
                .unwrap(),
            [
                "--profile",
                r#"C:\Users\Player One\game.ini"#,
                "--fullscreen"
            ]
        );
        assert!(split_command_arguments("\"unfinished").is_err());
    }

    #[test]
    fn launcher_supports_sparse_virtual_slot_indices() -> Result<(), Box<dyn Error>> {
        let options = arguments(&[])?;
        let mut launcher = LauncherState::from_options(&options);
        launcher.gamepads[0].slot = 3;
        let pads = launcher.gamepad_arguments()?;
        assert_eq!(pads.len(), 4);
        assert_eq!(pads[3].slot, 3);
        assert!(matches!(pads[0].selector, DeviceSelector::Name(_)));
        Ok(())
    }

    #[test]
    fn launcher_preserves_render_target_order_and_rotation() -> Result<(), Box<dyn Error>> {
        let options = arguments(&["--track-render-targets=4:1,2:3", "--graph-tracking"])?;
        let launcher = LauncherState::from_options(&options);
        assert_eq!(launcher.tracked_render_target_indices(), Some(vec![4, 2]));
        assert_eq!(launcher.render_target_rotations(), [1, 3]);
        assert!(launcher.graph_tracking);
        assert!(launcher.validate_render_targets().is_ok());
        Ok(())
    }

    #[test]
    fn accepts_stone_and_gamepad_configuration() -> Result<(), Box<dyn Error>> {
        let options = arguments(&[
            "--gamepad",
            "Nintendo Switch Controller:0:SWITCH_PRO_CONTROLLER",
            "--gamepad",
            "030000005e0400008e02000000000000:1:XBOX_CONTROLLER",
            "--stone-db",
            "stone.json",
            "--gamecontroller-db",
            "controllers.txt",
            "--",
            "game.exe",
        ])?;
        assert_eq!(
            options.gamepads[0],
            GamepadArgument {
                selector: DeviceSelector::Name("Nintendo Switch Controller".to_owned()),
                slot: 0,
                layout: "SWITCH_PRO_CONTROLLER".to_owned(),
                force_mouse: false,
            }
        );
        assert!(matches!(
            options.gamepads[1].selector,
            DeviceSelector::Guid(_)
        ));
        assert_eq!(options.stone_db, Some(PathBuf::from("stone.json")));
        assert_eq!(
            options.gamecontroller_db,
            Some(PathBuf::from("controllers.txt"))
        );
        let forced_mouse = "0+M:0:WII_REMOTE_CONTROLLER".parse::<GamepadArgument>()?;
        assert_eq!(forced_mouse.selector, DeviceSelector::Index(0));
        assert!(forced_mouse.force_mouse);
        assert_eq!(forced_mouse.layout, "WII_REMOTE_CONTROLLER");
        let keyboard = "K:0:WII_REMOTE_CONTROLLER".parse::<GamepadArgument>()?;
        assert_eq!(keyboard.selector, DeviceSelector::Keyboard);
        assert!(keyboard.force_mouse);
        assert!(arguments(&["--gamepad", "0:1:XBOX_CONTROLLER", "--", "game.exe"]).is_err());
        assert!(
            arguments(&[
                "--gamepad",
                "0:0:XBOX_CONTROLLER",
                "--gamepad",
                "1:1:XBOX_CONTROLLER",
                "--gamepad",
                "2:2:XBOX_CONTROLLER",
                "--gamepad",
                "3:3:XBOX_CONTROLLER",
                "--gamepad",
                "4:4:XBOX_CONTROLLER",
                "--",
                "game.exe",
            ])
            .is_ok()
        );
        Ok(())
    }

    #[test]
    fn hook_selection_defaults_to_auto_without_gdi() -> Result<(), Box<dyn Error>> {
        assert_eq!(
            arguments(&["--hook", "auto", "--", "game.exe"])?.hooks,
            AUTO_HOOKS
        );
        assert!(!AUTO_HOOKS.contains(&HookBackend::Gdi));
        Ok(())
    }

    #[test]
    fn hook_order_defines_priority() -> Result<(), Box<dyn Error>> {
        let options = arguments(&["--hook", "gdi,d3d11", "--hook", "vulkan", "--", "game.exe"])?;
        assert_eq!(
            options.hooks,
            [HookBackend::Gdi, HookBackend::D3d11, HookBackend::Vulkan]
        );
        let gdi = HookBackend::Gdi.class();
        let vulkan = HookBackend::Vulkan.class();
        assert!(class_priority(&options.hooks, gdi) < class_priority(&options.hooks, vulkan));
        assert_eq!(
            class_priority(&options.hooks, HookBackend::OpenGl.class()),
            None
        );
        Ok(())
    }

    #[test]
    fn hook_auto_appends_the_remaining_backends() -> Result<(), Box<dyn Error>> {
        assert_eq!(
            arguments(&["--hook=gdi,auto", "--", "game.exe"])?.hooks,
            [
                HookBackend::Gdi,
                HookBackend::Vulkan,
                HookBackend::OpenGl,
                HookBackend::D3d12,
                HookBackend::D3d11,
            ]
        );
        Ok(())
    }

    #[test]
    fn rejects_unknown_and_repeated_hooks() {
        assert!(arguments(&["--hook", "metal", "--", "game.exe"]).is_err());
        assert!(arguments(&["--hook", "vulkan,vulkan", "--", "game.exe"]).is_err());
    }

    #[test]
    fn accepts_vulkan_override_and_options_in_either_order() -> Result<(), Box<dyn Error>> {
        let options = arguments(&[
            "--track-render-target",
            "--no-isolate",
            "--force-km",
            "--api=vulkan",
            "--",
            "game.exe",
            "--api",
            "target-argument",
        ])?;
        assert_eq!(options.api, ViewerApi::Vulkan);
        assert_eq!(options.track_render_targets, Some(vec![tracked(0, 0)]));
        assert!(!options.isolate);
        assert!(options.force_keyed_mutex);
        assert_eq!(options.target, ["game.exe", "--api", "target-argument"]);
        Ok(())
    }

    #[test]
    fn accepts_render_target_indices_and_defaults_to_zero() -> Result<(), Box<dyn Error>> {
        assert_eq!(
            arguments(&["--track-render-targets", "0,1", "--", "game.exe"])?.track_render_targets,
            Some(vec![tracked(0, 0), tracked(1, 0)])
        );
        assert_eq!(
            arguments(&["--track-render-targets", "--", "game.exe"])?.track_render_targets,
            Some(vec![tracked(0, 0)])
        );
        assert_eq!(
            arguments(&["--track-render-targets=1", "--", "game.exe"])?.track_render_targets,
            Some(vec![tracked(1, 0)])
        );
        Ok(())
    }

    #[test]
    fn accepts_render_target_rotations_in_quarter_turns() -> Result<(), Box<dyn Error>> {
        assert_eq!(
            arguments(&["--track-render-targets", "0:1,1", "--", "game.exe"])?.track_render_targets,
            Some(vec![tracked(0, 1), tracked(1, 0)])
        );
        // Quarter turns wrap around the unit circle.
        assert_eq!(
            arguments(&["--track-render-targets=0:4,1:7", "--", "game.exe"])?.track_render_targets,
            Some(vec![tracked(0, 0), tracked(1, 3)])
        );
        Ok(())
    }

    #[test]
    fn rejects_invalid_render_target_indices() {
        assert!(arguments(&["--track-render-targets", "16", "--", "game.exe"]).is_err());
        assert!(arguments(&["--track-render-targets=0,0", "--", "game.exe"]).is_err());
        assert!(arguments(&["--track-render-targets=0:x", "--", "game.exe"]).is_err());
        assert!(arguments(&["--track-render-targets=0:", "--", "game.exe"]).is_err());
        // Rotation does not distinguish otherwise duplicate indices.
        assert!(arguments(&["--track-render-targets=0:1,0:2", "--", "game.exe"]).is_err());
    }

    #[test]
    fn accepts_webrtc_defaults_and_target_indices() -> Result<(), Box<dyn Error>> {
        let defaults = arguments(&["--webrtc", "--", "game.exe"])?;
        assert_eq!(defaults.webrtc, Some(vec![0]));
        let explicit = arguments(&[
            "--webrtc=0,1",
            "--bind-webrtc",
            "0.0.0.0:9000",
            "--",
            "game.exe",
        ])?;
        assert_eq!(explicit.webrtc, Some(vec![0, 1]));
        assert_eq!(explicit.bind_webrtc, "0.0.0.0:9000".parse()?);
        Ok(())
    }

    #[test]
    fn rejects_invalid_webrtc_options() {
        assert!(arguments(&["--webrtc=16", "--", "game.exe"]).is_err());
        assert!(arguments(&["--webrtc=0,0", "--", "game.exe"]).is_err());
        assert!(arguments(&["--bind-webrtc=0.0.0.0:9000", "--", "game.exe"]).is_err());
    }

    #[test]
    fn accepts_a_surface_count_and_defaults_to_one() -> Result<(), Box<dyn Error>> {
        assert_eq!(
            arguments(&["--surfaces", "2", "--", "game.exe"])?.surfaces,
            2
        );
        assert_eq!(arguments(&["--surfaces", "--", "game.exe"])?.surfaces, 16);
        assert_eq!(arguments(&["--surfaces=3", "--", "game.exe"])?.surfaces, 3);
        Ok(())
    }

    #[test]
    fn rejects_an_invalid_surface_count() {
        assert!(arguments(&["--surfaces", "0", "--", "game.exe"]).is_err());
        assert!(arguments(&["--surfaces=17", "--", "game.exe"]).is_err());
    }

    #[test]
    fn accepts_a_surface_skip_count_and_defaults_to_one() -> Result<(), Box<dyn Error>> {
        assert_eq!(
            arguments(&["--skip-surfaces", "2", "--", "game.exe"])?.skip_surfaces,
            2
        );
        assert_eq!(
            arguments(&["--skip-surfaces", "--", "game.exe"])?.skip_surfaces,
            1
        );
        assert_eq!(
            arguments(&["--skip-windows=3", "--", "game.exe"])?.skip_surfaces,
            3
        );
        Ok(())
    }

    #[test]
    fn rejects_an_invalid_surface_skip_count() {
        assert!(arguments(&["--skip-surfaces=menu", "--", "game.exe"]).is_err());
    }

    #[test]
    fn originating_api_stream_wins_same_window_duplicates() {
        let d3d11 = stream_backend_class(ExternalImageKind::D3d11D3d12Resource);
        let d3d12 = stream_backend_class(ExternalImageKind::D3d12Resource);
        let opengl = stream_backend_class(ExternalImageKind::OpenGlD3d12Resource);
        let vulkan = stream_backend_class(ExternalImageKind::VulkanD3d12Resource);
        assert!(should_replace_same_window_backend(
            &AUTO_HOOKS,
            d3d11,
            d3d12
        ));
        assert!(should_replace_same_window_backend(
            &AUTO_HOOKS,
            d3d12,
            opengl
        ));
        assert!(should_replace_same_window_backend(
            &AUTO_HOOKS,
            d3d12,
            vulkan
        ));
        assert!(!should_replace_same_window_backend(
            &AUTO_HOOKS,
            vulkan,
            d3d12
        ));
        assert!(!should_replace_same_window_backend(
            &AUTO_HOOKS,
            opengl,
            d3d12
        ));
        // A recreated surface from the same backend still replaces its predecessor.
        assert!(should_replace_same_window_backend(
            &AUTO_HOOKS,
            vulkan,
            vulkan
        ));
        // The submitted hook order overrides the automatic ranking.
        let reversed = [HookBackend::D3d11, HookBackend::Vulkan];
        assert!(!should_replace_same_window_backend(
            &reversed, d3d11, vulkan
        ));
        assert!(should_replace_same_window_backend(&reversed, vulkan, d3d11));
    }

    #[test]
    fn recreated_logical_surface_replaces_its_previous_connection() {
        let active = HashMap::from([(7, (graph_surface_for_test(1), 4))]);

        assert_eq!(
            matching_surface_connection(&active, graph_surface_for_test(1)),
            Some((7, 4))
        );
        assert_eq!(
            matching_surface_connection(&active, graph_surface_for_test(2)),
            None
        );
    }

    #[test]
    fn graph_surface_identity_determines_display_and_rotation_slot() {
        assert_eq!(graph_surface_index(graph_surface_for_test(0)), Some(0));
        assert_eq!(graph_surface_index(graph_surface_for_test(1)), Some(1));
        assert_eq!(graph_surface_index(0x1234), None);
    }

    fn graph_surface_for_test(index: u64) -> u64 {
        0x5347_0000_0000_0000 | index
    }

    fn offset_tuples(offsets: [vk::Offset3D; 2]) -> [(i32, i32, i32); 2] {
        offsets.map(|offset| (offset.x, offset.y, offset.z))
    }

    #[test]
    fn bottom_up_streams_reverse_vulkan_source_y() {
        assert_eq!(
            offset_tuples(blit_source_offsets(1280, 720, true, 0)),
            [(0, 720, 0), (1280, 0, 1)]
        );
        assert_eq!(
            offset_tuples(blit_source_offsets(1280, 720, false, 0)),
            [(0, 0, 0), (1280, 720, 1)]
        );
        assert_eq!(vertical_layout_index(0, 2, true), 1);
        assert_eq!(vertical_layout_index(1, 2, true), 0);
        assert_eq!(vertical_layout_index(0, 2, false), 0);
    }

    #[test]
    fn half_turns_mirror_vulkan_blit_source_offsets() {
        assert_eq!(
            offset_tuples(blit_source_offsets(1280, 720, false, 2)),
            [(1280, 720, 0), (0, 0, 1)]
        );
        // A flipped half turn cancels the vertical mirror.
        assert_eq!(
            offset_tuples(blit_source_offsets(1280, 720, true, 2)),
            [(1280, 0, 0), (0, 720, 1)]
        );
    }
}
