use clap::Parser;
use snowglass_pluginhost::{PluginRegistry, ThemePluginInstance};
use std::{error::Error, fs, path::PathBuf};

#[derive(Parser)]
#[command(name = "snowglass-gui", version, about)]
struct ShellArguments {
    /// Theme plugin ID (temporarily overrides localStorage selection).
    #[arg(long, value_name = "ID")]
    theme: Option<String>,

    /// Start and connect to the selected theme's development server.
    #[arg(long)]
    debug: bool,
}

pub struct ShellOptions {
    pub theme: crate::theme::SelectedTheme,
    pub plugins: PluginRegistry,
    pub directories: snowglass_library::Directories,
    pub local_storage: Option<snowglass_deno::webstorage::LocalStorage>,
    pub library: Option<snowglass_runtime::LibraryService>,
}

pub async fn parse() -> Result<ShellOptions, Box<dyn Error>> {
    let arguments = ShellArguments::parse();
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
    let directories = snowglass_library::Directories::discover()?;
    let local_storage = match snowglass_deno::webstorage::LocalStorage::open(&directories).await {
        Ok(storage) => Some(storage),
        Err(error) => {
            eprintln!("snowglass: could not open local storage: {error}");
            None
        }
    };
    let library =
        match snowglass_runtime::LibraryService::open_with_plugins(&directories, plugins.clone())
            .await
        {
            Ok(library) => Some(library),
            Err(error) => {
                eprintln!("snowglass: could not open games library: {error}");
                None
            }
        };
    let saved_theme = saved_theme(local_storage.as_ref()).await;
    let theme = select_theme(&plugins, arguments.theme.as_deref(), saved_theme.as_deref())?;
    let theme = crate::theme::SelectedTheme::new(theme, arguments.debug);
    Ok(ShellOptions {
        theme,
        plugins,
        directories,
        local_storage,
        library,
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

async fn saved_theme(storage: Option<&snowglass_deno::webstorage::LocalStorage>) -> Option<String> {
    let storage = storage?;
    match storage.get_item(crate::theme::STORAGE_KEY).await {
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
        if requested.is_empty()
            || !requested
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(format!("invalid theme plugin ID '{requested}'").into());
        }
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
main = "dist/index.html"

[theme]
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
