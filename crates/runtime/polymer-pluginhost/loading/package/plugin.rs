use serde::{Deserialize, de::DeserializeOwned};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};
use thiserror::Error;

/// Keys owned by the generic `[plugin]` block.
#[derive(Debug, Deserialize)]
struct PluginBlock {
    id: String,
    version: String,
    class: String,
    main: PathBuf,
    #[serde(default)]
    caches: Vec<PluginCache>,
    #[serde(default)]
    permissions: PluginPermissions,
    #[serde(default)]
    config: super::PluginConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginPermissions {
    #[serde(default)]
    net: NetworkPermissions,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct NetworkPermissions {
    #[serde(default)]
    allowed_hosts: Vec<String>,
    /// Allowed hosts whose certificates are not verified, for hosts whose certificates
    /// cannot be, such as chains signed with SHA-1 or certificates without a
    /// subjectAltName. Requests to them can be intercepted.
    #[serde(default)]
    insecure_hosts: Vec<String>,
}

impl NetworkPermissions {
    fn insecure_hosts(&self) -> Result<Vec<String>, PackageError> {
        match self
            .insecure_hosts
            .iter()
            .find(|host| !self.allowed_hosts.contains(host))
        {
            Some(host) => Err(PackageError::InsecureHost(host.clone())),
            None => Ok(self.insecure_hosts.clone()),
        }
    }
}

/// Persistent, plugin-local cache storage declared by a package.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct PluginCache {
    /// Stable key used by the plugin when requesting the cache resource.
    pub key: String,
    /// Human-readable name suitable for cache-management UI.
    pub label: String,
}

impl PluginCache {
    fn validate_all(caches: &[Self]) -> Result<(), PackageError> {
        let mut keys = BTreeSet::new();
        for cache in caches {
            if cache.key.is_empty()
                || !cache
                    .key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            {
                return Err(PackageError::InvalidCacheKey(cache.key.clone()));
            }
            if cache.label.trim().is_empty() {
                return Err(PackageError::InvalidCacheLabel(cache.key.clone()));
            }
            if !keys.insert(cache.key.clone()) {
                return Err(PackageError::DuplicateCache(cache.key.clone()));
            }
        }
        Ok(())
    }
}

/// Generic data shared by every loaded plugin package. `B` is the root block
/// belonging to the plugin class, such as `[emulator]`.
pub struct PluginPackage<B> {
    root: PathBuf,
    main: PathBuf,
    plugin: PluginBlock,
    block: B,
    manifest: serde_json::Value,
    module_loader: crate::loading::PluginModuleLoader,
    entrypoint: deno_core::ModuleSpecifier,
}

pub trait PluginClass: DeserializeOwned + Send + Sync + Sized + 'static {
    const CLASS: &'static str;

    fn prepare(package: &mut PluginPackage<Self>) -> Result<(), PackageError>;
}

impl<B: DeserializeOwned> PluginPackage<B> {
    fn load_manifest(root: impl AsRef<Path>) -> Result<Self, PackageError> {
        let root = root.as_ref().canonicalize()?;
        let manifest_path = root.join("plugin.toml");
        let mut manifest = fs::read_to_string(&manifest_path)?.parse::<toml::Table>()?;
        let document = serde_json::to_value(&manifest).expect("TOML tables are JSON-compatible");
        let plugin: PluginBlock = manifest
            .remove("plugin")
            .ok_or_else(|| PackageError::MissingBlock("plugin".to_owned()))?
            .try_into()?;
        if plugin.id.is_empty()
            || !plugin
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(PackageError::InvalidPluginId(plugin.id));
        }
        PluginCache::validate_all(&plugin.caches)?;
        plugin.config.validate()?;
        if plugin.main.as_os_str().is_empty()
            || plugin.main.is_absolute()
            || plugin
                .main
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(PackageError::InvalidPluginMain(plugin.main));
        }
        polymer_deno::Runtime::network_permissions(&plugin.permissions.net.allowed_hosts)
            .map_err(|error| PackageError::ModuleSpecifier(error.to_string()))?;
        let module_loader = crate::loading::PluginModuleLoader::new(&plugin.id, root.clone())
            .with_network_hosts(plugin.permissions.net.allowed_hosts.clone())
            .with_insecure_hosts(plugin.permissions.net.insecure_hosts()?);
        let entrypoint = module_loader
            .package_specifier(&plugin.main)
            .map_err(|error| PackageError::ModuleSpecifier(error.to_string()))?;
        let main = root.join(&plugin.main);
        let class = plugin.class.clone();
        let block: B = manifest
            .remove(&class)
            .ok_or_else(|| PackageError::MissingBlock(class.clone()))?
            .try_into()?;
        Ok(Self {
            root,
            main,
            plugin,
            block,
            manifest: document,
            module_loader,
            entrypoint,
        })
    }
}

impl<B: PluginClass> PluginPackage<B> {
    pub fn load(root: impl AsRef<Path>) -> Result<Self, PackageError> {
        let mut package = Self::load_manifest(root)?;
        if package.plugin.class != B::CLASS {
            return Err(PackageError::UnexpectedClass {
                expected: B::CLASS,
                actual: package.plugin.class.clone(),
            });
        }
        B::prepare(&mut package)?;
        Ok(package)
    }
}

impl<B> PluginPackage<B> {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn main(&self) -> &Path {
        &self.main
    }

    pub fn block(&self) -> &B {
        &self.block
    }

    pub(super) fn block_mut(&mut self) -> &mut B {
        &mut self.block
    }
}

impl<B> crate::plugin::PluginInstance for PluginPackage<B>
where
    B: Send + Sync + 'static,
{
    fn id(&self) -> &str {
        &self.plugin.id
    }

    fn version(&self) -> &str {
        &self.plugin.version
    }

    fn class(&self) -> &str {
        &self.plugin.class
    }

    fn module_loader(&self) -> &crate::loading::PluginModuleLoader {
        &self.module_loader
    }

    fn entrypoint(&self) -> &deno_core::ModuleSpecifier {
        &self.entrypoint
    }

    fn caches(&self) -> &[PluginCache] {
        &self.plugin.caches
    }

    fn config(&self) -> &super::PluginConfig {
        &self.plugin.config
    }

    fn manifest(&self) -> &serde_json::Value {
        &self.manifest
    }
}

pub(super) fn confined_path(root: &Path, path: impl AsRef<Path>) -> Result<PathBuf, PackageError> {
    let path = if path.as_ref().is_absolute() {
        path.as_ref().canonicalize()?
    } else {
        root.join(path).canonicalize()?
    };
    if !path.starts_with(root) {
        return Err(PackageError::EscapesPackage(path));
    }
    Ok(path)
}

#[derive(Debug, Error)]
pub enum PackageError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Manifest(#[from] toml::de::Error),
    #[error(transparent)]
    GlobPattern(#[from] glob::PatternError),
    #[error(transparent)]
    Glob(#[from] glob::GlobError),
    #[error(transparent)]
    Configuration(#[from] polymer_config::error::Error),
    #[error("package path is not UTF-8: {}", .0.display())]
    NonUtf8Path(PathBuf),
    #[error("configuration definition pattern matched no files: {0}")]
    EmptyDefinitionPattern(String),
    #[error("package path escapes its root: {}", .0.display())]
    EscapesPackage(PathBuf),
    #[error("insecure host '{0}' is not an allowed host")]
    InsecureHost(String),
    #[error("emulator package has no input definition")]
    NoInputDefinition,
    #[error("emulator package does not support platform '{0}'")]
    UnknownPlatform(String),
    #[error("invalid plugin ID '{0}'")]
    InvalidPluginId(String),
    #[error("plugin manifest has no [{0}] block")]
    MissingBlock(String),
    #[error("plugin class '{actual}' does not match expected class '{expected}'")]
    UnexpectedClass {
        expected: &'static str,
        actual: String,
    },
    #[error("plugin module specifier: {0}")]
    ModuleSpecifier(String),
    #[error(transparent)]
    Vfs(#[from] polymer_vfs::VfsError),
    #[error("invalid plugin cache key '{0}'")]
    InvalidCacheKey(String),
    #[error("plugin cache '{0}' must have a non-empty label")]
    InvalidCacheLabel(String),
    #[error("duplicate plugin cache '{0}'")]
    DuplicateCache(String),
    #[error("plugin config option '{0}' needs a display name and a default among its choices")]
    InvalidConfigOption(String),
    #[error("invalid namespaced scrape key '{0}'")]
    InvalidScrapeKey(String),
    #[error("duplicate scraper provides key '{0}'")]
    DuplicateScrapeKey(String),
    #[error("invalid scraper requirement '{0}'")]
    InvalidScrapeRequirement(String),
    #[error("emulator save-state revision must not be empty")]
    InvalidSaveStateRevision,
    #[error("plugin main must be a confined package-relative path: {}", .0.display())]
    InvalidPluginMain(PathBuf),
    #[error("theme debug serve command must not be empty")]
    InvalidThemeServe,
    #[error("theme debug URL must be an absolute HTTP(S) URL: {0}")]
    InvalidThemeDebugUrl(String),
    #[error(
        "layout '{layout}' does not identify one next port on '{console}' (candidates: {candidates:?})"
    )]
    AmbiguousLayout {
        console: String,
        layout: String,
        candidates: Vec<String>,
    },
}
