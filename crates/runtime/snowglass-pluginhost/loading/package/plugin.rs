use serde::{Deserialize, de::DeserializeOwned};
use snowglass_vfs::Permissions;
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
    resources: Vec<ResourceBlock>,
    #[serde(default)]
    caches: Vec<PluginCache>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PluginResourceFormat {
    Directory,
    Asar,
    File,
}

#[derive(Debug, Deserialize)]
struct ResourceBlock {
    name: String,
    source: String,
    format: PluginResourceFormat,
    mode: String,
    #[serde(default)]
    target: Option<String>,
}

impl ResourceBlock {
    fn validate_all(root: &Path, resources: &[Self]) -> Result<(), PackageError> {
        let mut keys = BTreeSet::new();
        for resource in resources {
            if resource.name.is_empty()
                || !resource
                    .name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            {
                return Err(PackageError::InvalidResourceName(resource.name.clone()));
            }
            if !keys.insert((resource.name.clone(), resource.target.clone())) {
                return Err(PackageError::DuplicateResource {
                    name: resource.name.clone(),
                    target: resource.target.clone(),
                });
            }
            let parsed = resource.materialize(root)?;
            match parsed.format {
                PluginResourceFormat::Directory if !parsed.path.is_dir() => {
                    return Err(PackageError::InvalidResourceType(resource.name.clone()));
                }
                PluginResourceFormat::Asar | PluginResourceFormat::File
                    if !parsed.path.is_file() =>
                {
                    return Err(PackageError::InvalidResourceType(resource.name.clone()));
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn materialize(&self, root: &Path) -> Result<PluginResource, PackageError> {
        let source = self
            .source
            .strip_prefix("pkg:")
            .ok_or_else(|| PackageError::InvalidResourceSource(self.source.clone()))?;
        let relative = Path::new(source);
        if source.is_empty()
            || source.starts_with('/')
            || source.contains(['\\', '?', '#'])
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(PackageError::InvalidResourceSource(self.source.clone()));
        }
        let permissions = self.mode.parse()?;
        let authority: Permissions = "rx".parse().expect("canonical permission constant");
        if !authority.contains(permissions) {
            return Err(PackageError::Vfs(
                snowglass_vfs::VfsError::AuthorityExceeded {
                    authority,
                    requested: permissions,
                },
            ));
        }
        Ok(PluginResource {
            name: self.name.clone(),
            path: confined_path(root, relative)?,
            format: self.format,
            permissions,
        })
    }

    fn current_target() -> String {
        let architecture = std::env::consts::ARCH;
        let suffix = if cfg!(target_os = "windows") {
            "pc-windows-msvc"
        } else if cfg!(target_os = "linux") {
            "unknown-linux-gnu"
        } else {
            std::env::consts::OS
        };
        format!("{architecture}-{suffix}")
    }
}

#[derive(Clone, Debug)]
pub struct PluginResource {
    pub name: String,
    pub path: PathBuf,
    pub format: PluginResourceFormat,
    pub permissions: Permissions,
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
        ResourceBlock::validate_all(&root, &plugin.resources)?;
        PluginCache::validate_all(&plugin.caches)?;
        if plugin.main.as_os_str().is_empty()
            || plugin.main.is_absolute()
            || plugin
                .main
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(PackageError::InvalidPluginMain(plugin.main));
        }
        let module_loader = crate::loading::PluginModuleLoader::new(&plugin.id, root.clone());
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

    fn resources(&self) -> Result<Vec<PluginResource>, PackageError> {
        let target = ResourceBlock::current_target();
        let mut names = BTreeSet::new();
        let mut selected = Vec::new();
        for resource in self
            .plugin
            .resources
            .iter()
            .filter(|resource| resource.target.as_deref() == Some(target.as_str()))
        {
            names.insert(resource.name.clone());
            selected.push(resource.materialize(&self.root)?);
        }
        for resource in self
            .plugin
            .resources
            .iter()
            .filter(|resource| resource.target.is_none() && !names.contains(&resource.name))
        {
            selected.push(resource.materialize(&self.root)?);
        }
        Ok(selected)
    }

    fn caches(&self) -> &[PluginCache] {
        &self.plugin.caches
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
    Configuration(#[from] snowglass_config::error::Error),
    #[error("package path is not UTF-8: {}", .0.display())]
    NonUtf8Path(PathBuf),
    #[error("configuration definition pattern matched no files: {0}")]
    EmptyDefinitionPattern(String),
    #[error("package path escapes its root: {}", .0.display())]
    EscapesPackage(PathBuf),
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
    Vfs(#[from] snowglass_vfs::VfsError),
    #[error("invalid plugin resource name '{0}'")]
    InvalidResourceName(String),
    #[error("duplicate plugin resource '{name}' for target {target:?}")]
    DuplicateResource {
        name: String,
        target: Option<String>,
    },
    #[error("plugin resource source must be a confined pkg: URL: {0}")]
    InvalidResourceSource(String),
    #[error("plugin resource '{0}' does not match its declared format")]
    InvalidResourceType(String),
    #[error("invalid plugin cache key '{0}'")]
    InvalidCacheKey(String),
    #[error("plugin cache '{0}' must have a non-empty label")]
    InvalidCacheLabel(String),
    #[error("duplicate plugin cache '{0}'")]
    DuplicateCache(String),
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
