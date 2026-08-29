use crate::{
    input::GamepadMapping,
    launch::{GraphicsHook, UnattachedLaunchPlan, UnattachedTrackedSurface},
};
use clap::{Arg, ArgAction};
use snowglass_pluginhost::{EmulatorPluginInstance, PluginRegistry, ThemePluginInstance};
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
    pub theme: crate::theme::SelectedTheme,
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
        graph_tracking: options.graph_tracking,
        surfaces,
        gamepad_mapping: options.gamepads.iter().map(GamepadMapping::from).collect(),
        machine: options.machine.clone(),
        webrtc_bind: options
            .webrtc
            .as_ref()
            .map(|_| options.bind_webrtc.to_string()),
        prepared_filesystem: None,
        controller_session: None,
    };
    launch.validate()?;
    Ok(Some(launch))
}

pub fn parse() -> Result<ShellOptions, Box<dyn Error>> {
    let matches = viewer_command_named("snowglass-gui")
        .arg(
            Arg::new("theme")
                .long("theme")
                .value_name("ID")
                .help("Theme plugin ID (temporarily overrides localStorage selection)"),
        )
        .arg(
            Arg::new("debug")
                .long("debug")
                .action(ArgAction::SetTrue)
                .help("Start and connect to the selected theme's development server"),
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
        for manifest in plugin_manifests(&plugin_root)? {
            if let Err(error) = plugins.load(&manifest) {
                eprintln!("snowglass: skipping plugin {}: {error}", manifest.display());
            }
        }
    }
    let requested_theme = matches.get_one::<String>("theme").map(String::as_str);
    let theme = select_theme(&plugins, requested_theme, saved_theme().as_deref())?;
    let theme = crate::theme::SelectedTheme::new(theme, matches.get_flag("debug"));
    let emulator = matches
        .get_one::<String>("emulator")
        .map(|emulator| -> Result<_, Box<dyn Error>> {
            validate_plugin_id(emulator, "emulator")?;
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
        theme,
        plugins,
        game_launch,
    })
}

fn plugin_manifests(root: &std::path::Path) -> Result<Vec<PathBuf>, std::io::Error> {
    let mut pending = vec![root.to_owned()];
    let mut manifests = Vec::new();
    while let Some(directory) = pending.pop() {
        let manifest = directory.join("plugin.toml");
        if directory != root && manifest.is_file() {
            manifests.push(manifest);
            continue;
        }
        let mut children = fs::read_dir(&directory)?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                entry
                    .file_type()
                    .ok()
                    .filter(|kind| kind.is_dir())
                    .map(|_| entry.path())
            })
            .collect::<Vec<_>>();
        children.sort();
        pending.extend(children.into_iter().rev());
    }
    manifests.sort();
    Ok(manifests)
}

fn saved_theme() -> Option<String> {
    let storage = crate::theme::web_storage_dir()?;
    match snowglass_deno::read_local_storage(storage, crate::theme::STORAGE_KEY) {
        Ok(theme) => theme,
        Err(error) => {
            eprintln!("snowglass: could not read saved theme selection: {error}");
            None
        }
    }
}

fn select_theme(
    plugins: &PluginRegistry,
    requested: Option<&str>,
    saved: Option<&str>,
) -> Result<std::sync::Arc<dyn ThemePluginInstance>, Box<dyn Error>> {
    if let Some(requested) = requested {
        validate_plugin_id(requested, "theme")?;
        return plugins
            .plugin::<dyn ThemePluginInstance>(requested)
            .ok_or_else(|| format!("plugin '{requested}' is not a loaded theme plugin").into());
    }
    if let Some(saved) = saved.filter(|saved| !saved.is_empty()) {
        if let Some(theme) = plugins.plugin::<dyn ThemePluginInstance>(saved) {
            return Ok(theme);
        }
        eprintln!("snowglass: saved theme '{saved}' is unavailable; using the first loaded theme");
    }
    plugins
        .plugins::<dyn ThemePluginInstance>()
        .into_iter()
        .next()
        .ok_or_else(|| "no theme plugins are loaded".into())
}

fn validate_plugin_id(id: &str, kind: &str) -> Result<(), Box<dyn Error>> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(format!("invalid {kind} plugin ID '{id}'").into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;

    fn write_theme(root: &Path, id: &str) {
        fs::create_dir_all(root).unwrap();
        fs::write(
            root.join("plugin.toml"),
            format!(
                r#"[plugin]
id = "{id}"
version = "0"
class = "theme"

[theme]
main = "dist/index.html"
"#,
            ),
        )
        .unwrap();
    }

    #[test]
    fn discovers_grouped_packages_without_descending_into_a_package() {
        let plugins = TempDir::new().unwrap();
        write_theme(&plugins.path().join("theme/zulu"), "zulu");
        write_theme(&plugins.path().join("theme/alpha"), "alpha");
        write_theme(
            &plugins.path().join("theme/zulu/node_modules/nested"),
            "nested",
        );

        let manifests = plugin_manifests(plugins.path()).unwrap();
        assert_eq!(manifests.len(), 2);
        assert!(manifests[0].ends_with("theme/alpha/plugin.toml"));
        assert!(manifests[1].ends_with("theme/zulu/plugin.toml"));
    }

    #[test]
    fn selects_cli_then_saved_then_first_theme() {
        let packages = TempDir::new().unwrap();
        write_theme(&packages.path().join("alpha"), "alpha");
        write_theme(&packages.path().join("zulu"), "zulu");
        let plugins = PluginRegistry::new();
        plugins
            .load(packages.path().join("alpha/plugin.toml"))
            .unwrap();
        plugins
            .load(packages.path().join("zulu/plugin.toml"))
            .unwrap();

        assert_eq!(
            select_theme(&plugins, Some("zulu"), Some("alpha"))
                .unwrap()
                .id(),
            "zulu"
        );
        assert_eq!(
            select_theme(&plugins, None, Some("zulu")).unwrap().id(),
            "zulu"
        );
        assert_eq!(
            select_theme(&plugins, None, Some("missing")).unwrap().id(),
            "alpha"
        );
        assert!(select_theme(&plugins, Some("missing"), None).is_err());
        assert!(select_theme(&plugins, Some("../bad"), None).is_err());
    }
}
