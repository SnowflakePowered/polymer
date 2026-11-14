use clap::Parser;
use polymer_pluginhost::{loading::PluginRegistry, theme::ThemePluginInstance};
use std::{error::Error, fs};

#[derive(Parser)]
#[command(name = "polymer-gui", version, about)]
pub(crate) struct ShellArguments {
    /// Theme plugin ID (temporarily overrides localStorage selection).
    #[arg(long, value_name = "ID")]
    theme: Option<String>,

    /// Start and connect to the selected theme's development server.
    #[arg(long)]
    debug: bool,

    /// Choose the theme in a standalone window, then start the shell with it.
    #[arg(long)]
    pick_theme: bool,
}

impl ShellArguments {
    pub(crate) fn pick_theme_requested() -> bool {
        Self::parse().pick_theme
    }

    pub(crate) fn discover_plugins(
        directories: &polymer_library::Directories,
    ) -> Result<PluginRegistry, Box<dyn Error>> {
        let plugins = PluginRegistry::new();
        let index = polymer_distribution::Index::read(&directories.plugin_index()).unwrap_or_else(|error| {
            eprintln!("polymer: could not read the plugin index: {error}");
            polymer_distribution::Index::default()
        });
        for (path, error) in &index.rejected {
            eprintln!("polymer: skipping plugin index entry {}: {error}", path.display());
        }
        let search_paths = [std::env::current_dir()?.join("plugins"), directories.packages()];
        for (path, error) in plugins.discover(&search_paths, &index) {
            eprintln!("polymer: skipping plugin {}: {error}", path.display());
        }
        Ok(plugins)
    }
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
    let directories = polymer_library::Directories::discover()?;
    let plugins = ShellArguments::discover_plugins(&directories)?;
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
