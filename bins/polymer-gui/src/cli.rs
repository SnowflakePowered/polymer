use clap::Parser;
use polymer_pluginhost::{loading::PluginRegistry, theme::ThemePluginInstance};
use std::{error::Error, fs, path::PathBuf};

#[derive(Parser)]
#[command(name = "polymer-gui", version, about)]
struct ShellArguments {
    /// Theme plugin ID (temporarily overrides localStorage selection).
    #[arg(long, value_name = "ID")]
    theme: Option<String>,

    /// Start and connect to the selected theme's development server.
    #[arg(long)]
    debug: bool,
}

pub struct ShellOptions {
    pub debug: bool,
    pub theme: crate::theme::SelectedTheme,
    pub plugins: PluginRegistry,
    pub directories: polymer_library::Directories,
    pub local_storage: Option<polymer_deno::webstorage::LocalStorage>,
    pub library: Option<polymer_runtime::LibraryService>,
    pub jobs: Option<std::sync::Arc<polymer_library::jobs::JobStore>>,
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
                eprintln!("polymer: skipping plugin {}: {error}", manifest.display());
            }
        }
    }
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent);
    for directory in workspace
        .and_then(|workspace| fs::read_dir(workspace.join("packages")).ok())
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
    {
        let name = fs::read(directory.join("jsr.json"))
            .ok()
            .and_then(|manifest| serde_json::from_slice::<serde_json::Value>(&manifest).ok())
            .and_then(|manifest| manifest.get("name")?.as_str().map(str::to_owned));
        if let Some(name) = name {
            plugins.override_jsr(name, directory);
        }
    }
    let directories = polymer_library::Directories::discover()?;
    let local_storage = match polymer_deno::webstorage::LocalStorage::open(&directories).await {
        Ok(storage) => Some(storage),
        Err(error) => {
            eprintln!("polymer: could not open local storage: {error}");
            None
        }
    };
    let jobs = match polymer_library::jobs::JobStore::open(directories.jobs_database()).await {
        Ok(jobs) => Some(jobs),
        Err(error) => {
            eprintln!("polymer: could not open jobs database: {error}");
            None
        }
    };
    let library = match &jobs {
        Some(jobs) => match polymer_runtime::LibraryService::open_with_plugins(
            &directories,
            plugins.clone(),
            jobs.clone(),
        )
        .await
        {
            Ok(library) => Some(library),
            Err(error) => {
                eprintln!("polymer: could not open games library: {error}");
                None
            }
        },
        None => None,
    };
    let saved_theme = saved_theme(local_storage.as_ref()).await;
    let theme = select_theme(&plugins, arguments.theme.as_deref(), saved_theme.as_deref())?;
    let theme = crate::theme::SelectedTheme::new(theme, arguments.debug);
    Ok(ShellOptions {
        debug: arguments.debug,
        theme,
        plugins,
        directories,
        local_storage,
        library,
        jobs,
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

async fn saved_theme(storage: Option<&polymer_deno::webstorage::LocalStorage>) -> Option<String> {
    let storage = storage?;
    match storage.get_item(crate::theme::STORAGE_KEY).await {
        Ok(theme) => theme,
        Err(error) => {
            eprintln!("polymer: could not read saved theme selection: {error}");
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
        eprintln!("polymer: saved theme '{saved}' is unavailable; using the first loaded theme");
    }
    plugins
        .plugins::<dyn ThemePluginInstance>()
        .into_iter()
        .next()
        .ok_or_else(|| "no theme plugins are loaded".into())
}
