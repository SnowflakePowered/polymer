use crate::commands::ShellDocumentSource;
use crate::shell::document::{ThemeFiles, ThemeLocation};
use deno_core::ModuleSpecifier;
use polymer_pluginhost::theme::ThemePluginInstance;
use polymer_pluginmanifest::PackageSource;
use std::{
    error::Error,
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

pub(crate) const STORAGE_KEY: &str = "Polymer.theme";
const DEBUG_STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const DEBUG_RETRY_INTERVAL: Duration = Duration::from_millis(100);

pub(crate) struct SelectedTheme {
    plugin: Arc<dyn ThemePluginInstance>,
    debug: bool,
    server: Option<ThemeServer>,
    url: Option<ModuleSpecifier>,
}

impl SelectedTheme {
    pub(crate) fn new(plugin: Arc<dyn ThemePluginInstance>, debug: bool) -> Self {
        Self {
            plugin,
            debug,
            server: None,
            url: None,
        }
    }

    pub(crate) fn titlebar(&self) -> bool {
        self.plugin.titlebar()
    }

    pub(crate) async fn load_document(&mut self) -> Result<ShellDocumentSource, Box<dyn Error>> {
        if self.debug {
            let debug = self.plugin.debug().cloned().ok_or_else(|| {
                format!(
                    "theme '{}' does not declare [theme.debug]",
                    self.plugin.id()
                )
            })?;
            let PackageSource::Directory(root) = self.plugin.source() else {
                return Err(format!(
                    "theme '{}' is archived; debug mode needs its unpacked directory",
                    self.plugin.id()
                )
                .into());
            };
            ensure_debug_address_available(&debug.url).await?;
            self.server = Some(ThemeServer::spawn(root, &debug.serve)?);
            self.url = Some(debug.url.clone());
            return self
                .load_debug_document(&debug.url)
                .await
                .map(|source| self.with_assets(source));
        }

        let unavailable = |error: &dyn std::fmt::Display| {
            format!(
                "theme '{}' production document {} is unavailable; run its frontend build: {error}",
                self.plugin.id(),
                self.plugin.main()
            )
        };
        self.plugin
            .source()
            .require_file(self.plugin.main())
            .map_err(|error| unavailable(&error))?;
        let url = match self.plugin.source() {
            PackageSource::Directory(_) => {
                let main = self
                    .plugin
                    .source()
                    .host_path(self.plugin.main())?
                    .expect("directory packages have host paths");
                ModuleSpecifier::from_file_path(&main)
                    .map_err(|_| format!("could not convert {} to a file URL", main.display()))?
            }
            PackageSource::Archive(_) => {
                let main = self.plugin.main();
                let page = match self.archived_root().as_str() {
                    "" => Some(main),
                    root => main
                        .strip_prefix(root)
                        .and_then(|page| page.strip_prefix('/')),
                }
                .ok_or_else(|| unavailable(&"it is outside the theme root"))?;
                ModuleSpecifier::parse(&format!("theme:///{page}"))?
            }
        };
        self.url = Some(url.clone());
        self.fetch_document(&url)
            .await
            .map(|source| self.with_assets(source))
    }

    /// The directory an archived theme serves: its `theme.root`, or the directory of its main
    /// document.
    fn archived_root(&self) -> String {
        match self.plugin.theme_root() {
            Some(root) => PackageSource::relative(root).unwrap_or_default(),
            None => self
                .plugin
                .main()
                .rsplit_once('/')
                .map(|(directory, _)| directory.to_owned())
                .unwrap_or_default(),
        }
    }

    async fn fetch_document(
        &self,
        url: &ModuleSpecifier,
    ) -> Result<ShellDocumentSource, Box<dyn Error>> {
        let html = match url.scheme() {
            "file" => {
                let path = url
                    .to_file_path()
                    .map_err(|_| format!("invalid theme file URL: {url}"))?;
                tokio::fs::read_to_string(path).await?
            }
            "theme" => self.plugin.source().read_to_string(self.plugin.main())?,
            _ => {
                reqwest::get(url.clone())
                    .await?
                    .error_for_status()?
                    .text()
                    .await?
            }
        };
        Ok(ShellDocumentSource {
            html,
            base_url: url.to_string(),
            theme: ThemeLocation::default(),
        })
    }

    fn with_assets(&self, source: ShellDocumentSource) -> ShellDocumentSource {
        let assets = self
            .plugin
            .assets()
            .map(|assets| PackageSource::relative(assets).unwrap_or_default());
        let theme = match self.plugin.source() {
            PackageSource::Directory(package) => ThemeLocation {
                root: self
                    .plugin
                    .theme_root()
                    .filter(|_| !self.debug)
                    .map(|root| ThemeFiles::Directory(package.join(root))),
                assets: assets.map(|assets| {
                    let directory = ThemeFiles::Directory(package.join(&assets));
                    (format!("{assets}/"), directory)
                }),
            },
            PackageSource::Archive(archive) => ThemeLocation {
                root: Some(ThemeFiles::Archive(archive.clone(), self.archived_root())),
                assets: assets.map(|assets| {
                    let directory = ThemeFiles::Archive(archive.clone(), assets.clone());
                    (format!("{assets}/"), directory)
                }),
            },
        };
        ShellDocumentSource { theme, ..source }
    }

    pub(crate) async fn reload_document(&self) -> Result<ShellDocumentSource, Box<dyn Error>> {
        let url = self
            .url
            .as_ref()
            .ok_or("the theme document was never loaded")?;
        self.fetch_document(url)
            .await
            .map(|source| self.with_assets(source))
    }

    async fn load_debug_document(
        &mut self,
        url: &ModuleSpecifier,
    ) -> Result<ShellDocumentSource, Box<dyn Error>> {
        let deadline = Instant::now() + DEBUG_STARTUP_TIMEOUT;
        loop {
            let request_error = match self.fetch_document(url).await {
                Ok(document) => {
                    if let Some(status) = self
                        .server
                        .as_mut()
                        .expect("debug server is retained while loading")
                        .try_wait()?
                    {
                        return Err(format!(
                            "theme '{}' debug server exited with {status} while starting {}",
                            self.plugin.id(),
                            url
                        )
                        .into());
                    }
                    return Ok(document);
                }
                Err(error) => error.to_string(),
            };
            if let Some(status) = self
                .server
                .as_mut()
                .expect("debug server is retained while loading")
                .try_wait()?
            {
                return Err(format!(
                    "theme '{}' debug server exited with {status} before {} became ready",
                    self.plugin.id(),
                    url
                )
                .into());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "theme '{}' debug server did not make {} ready within {} seconds: {}",
                    self.plugin.id(),
                    url,
                    DEBUG_STARTUP_TIMEOUT.as_secs(),
                    request_error
                )
                .into());
            }
            tokio::time::sleep(DEBUG_RETRY_INTERVAL).await;
        }
    }
}

async fn ensure_debug_address_available(url: &ModuleSpecifier) -> Result<(), Box<dyn Error>> {
    let host = url
        .host_str()
        .ok_or_else(|| format!("theme debug URL has no host: {url}"))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| format!("theme debug URL has no port: {url}"))?;
    if matches!(
        tokio::time::timeout(
            Duration::from_millis(250),
            tokio::net::TcpStream::connect((host, port))
        )
        .await,
        Ok(Ok(_))
    ) {
        return Err(format!(
            "theme debug address {host}:{port} is already in use; stop the existing server so Polymer can manage it"
        )
        .into());
    }
    Ok(())
}

struct ThemeServer {
    child: Child,
    #[cfg(windows)]
    job: polymer_platform_windows::KillOnCloseJob,
}

impl ThemeServer {
    fn spawn(root: &Path, serve: &str) -> Result<Self, Box<dyn Error>> {
        let root = command_path(root);
        #[cfg(windows)]
        let job = polymer_platform_windows::KillOnCloseJob::new()?;
        let mut command = platform_shell(serve);
        command
            .current_dir(&root)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        if let Some(path) = development_path(&root)? {
            command.env("PATH", path);
        }
        let mut child = command.spawn().map_err(|error| {
            format!(
                "could not start theme debug command '{serve}' in {}: {error}",
                root.display()
            )
        })?;

        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            if let Err(error) = job.assign_process(child.as_raw_handle() as isize) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.into());
            }
            Ok(Self { child, job })
        }
        #[cfg(not(windows))]
        {
            Ok(Self { child })
        }
    }

    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }
}

impl Drop for ThemeServer {
    fn drop(&mut self) {
        #[cfg(windows)]
        let _ = self.job.terminate(1);
        #[cfg(not(windows))]
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn development_path(root: &Path) -> Result<Option<OsString>, std::env::JoinPathsError> {
    let bin = root.join("node_modules").join(".bin");
    let current = std::env::var_os("PATH").unwrap_or_default();
    let paths = std::iter::once(bin).chain(std::env::split_paths(&current));
    std::env::join_paths(paths).map(Some)
}

fn command_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let path = path.to_string_lossy();
        if let Some(path) = path.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{path}"));
        }
        if let Some(path) = path.strip_prefix(r"\\?\") {
            return PathBuf::from(path);
        }
    }
    path.to_owned()
}

fn platform_shell(command: &str) -> Command {
    #[cfg(windows)]
    {
        let mut shell =
            Command::new(std::env::var_os("COMSPEC").unwrap_or_else(|| "cmd.exe".into()));
        shell.args(["/D", "/S", "/C"]).arg(command);
        shell
    }
    #[cfg(not(windows))]
    {
        let mut shell = Command::new("/bin/sh");
        shell.args(["-c", command]);
        shell
    }
}
