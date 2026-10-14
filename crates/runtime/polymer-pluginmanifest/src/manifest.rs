use serde::{Deserialize, de::DeserializeOwned};
use std::{collections::BTreeSet, path::PathBuf};
use thiserror::Error;

use crate::{EmulatorBlock, PackageSource, PlatformBlock, PluginConfig, ScraperBlock, ThemeBlock};

/// Keys owned by the generic `[plugin]` block.
#[derive(Debug, Deserialize)]
struct PluginBlock {
    id: String,
    version: String,
    class: String,
    main: PathBuf,
    api: semver::VersionReq,
    #[serde(default)]
    caches: Vec<PluginCache>,
    #[serde(default)]
    permissions: PluginPermissions,
    #[serde(default)]
    config: PluginConfig,
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
    fn validate(&self) -> Result<(), PackageError> {
        match self
            .insecure_hosts
            .iter()
            .find(|host| !self.allowed_hosts.contains(host))
        {
            Some(host) => Err(PackageError::InsecureHost(host.clone())),
            None => Ok(()),
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

/// A validated `plugin.toml`. `B` is the block belonging to the plugin class, such as
/// `[emulator]`.
pub struct PluginManifest<B> {
    source: PackageSource,
    main: String,
    plugin: PluginBlock,
    block: B,
    document: serde_json::Value,
}

pub trait PluginClass: DeserializeOwned + Send + Sync + Sized + 'static {
    const CLASS: &'static str;

    fn prepare(manifest: &mut PluginManifest<Self>) -> Result<(), PackageError>;
}

impl<B: PluginClass> PluginManifest<B> {
    /// Reads and validates the package's `plugin.toml`.
    pub fn load(source: PackageSource) -> Result<Self, PackageError> {
        let mut table = source
            .read_to_string("plugin.toml")?
            .parse::<toml::Table>()?;
        let document = serde_json::to_value(&table).expect("TOML tables are JSON-compatible");
        let plugin: PluginBlock = table
            .remove("plugin")
            .ok_or_else(|| PackageError::MissingBlock("plugin".to_owned()))?
            .try_into()?;
        crate::PluginId::parse(&plugin.id)?;
        if plugin.class != B::CLASS {
            return Err(PackageError::UnexpectedClass {
                expected: B::CLASS,
                actual: plugin.class,
            });
        }
        PluginCache::validate_all(&plugin.caches)?;
        plugin.config.validate()?;
        plugin.permissions.net.validate()?;
        let main = PackageSource::relative(&plugin.main)
            .map_err(|_| PackageError::InvalidPluginMain(plugin.main.clone()))?;
        let block: B = table
            .remove(B::CLASS)
            .ok_or_else(|| PackageError::MissingBlock(B::CLASS.to_owned()))?
            .try_into()?;
        let mut manifest = Self {
            source,
            main,
            plugin,
            block,
            document,
        };
        B::prepare(&mut manifest)?;
        Ok(manifest)
    }
}

impl<B> PluginManifest<B> {
    /// The directory or archive holding `plugin.toml`.
    pub fn source(&self) -> &PackageSource {
        &self.source
    }

    /// The package-relative, `/`-separated path of the entry point.
    pub fn main(&self) -> &str {
        &self.main
    }

    pub fn block(&self) -> &B {
        &self.block
    }

    pub(crate) fn block_mut(&mut self) -> &mut B {
        &mut self.block
    }

    pub fn id(&self) -> &str {
        &self.plugin.id
    }

    pub fn version(&self) -> &str {
        &self.plugin.version
    }

    pub fn class(&self) -> &str {
        &self.plugin.class
    }

    pub fn api(&self) -> &semver::VersionReq {
        &self.plugin.api
    }

    pub fn caches(&self) -> &[PluginCache] {
        &self.plugin.caches
    }

    pub fn config(&self) -> &PluginConfig {
        &self.plugin.config
    }

    pub fn allowed_hosts(&self) -> &[String] {
        &self.plugin.permissions.net.allowed_hosts
    }

    pub fn insecure_hosts(&self) -> &[String] {
        &self.plugin.permissions.net.insecure_hosts
    }

    /// The whole `plugin.toml`.
    pub fn document(&self) -> &serde_json::Value {
        &self.document
    }
}

/// A manifest of whichever class its `plugin.toml` declares.
pub enum AnyPluginManifest {
    Emulator(PluginManifest<EmulatorBlock>),
    Platform(PluginManifest<PlatformBlock>),
    Scraper(PluginManifest<ScraperBlock>),
    Theme(PluginManifest<ThemeBlock>),
}

impl AnyPluginManifest {
    /// Reads and validates the package's `plugin.toml` as the class it declares.
    pub fn load(source: PackageSource) -> Result<Self, PackageError> {
        #[derive(Deserialize)]
        struct Header {
            plugin: HeaderPlugin,
        }
        #[derive(Deserialize)]
        struct HeaderPlugin {
            class: String,
        }
        let header: Header = toml::from_str(&source.read_to_string("plugin.toml")?)?;
        Ok(match header.plugin.class.as_str() {
            EmulatorBlock::CLASS => Self::Emulator(PluginManifest::load(source)?),
            PlatformBlock::CLASS => Self::Platform(PluginManifest::load(source)?),
            ScraperBlock::CLASS => Self::Scraper(PluginManifest::load(source)?),
            ThemeBlock::CLASS => Self::Theme(PluginManifest::load(source)?),
            class => return Err(PackageError::UnknownClass(class.to_owned())),
        })
    }

    pub fn id(&self) -> &str {
        match self {
            Self::Emulator(manifest) => manifest.id(),
            Self::Platform(manifest) => manifest.id(),
            Self::Scraper(manifest) => manifest.id(),
            Self::Theme(manifest) => manifest.id(),
        }
    }

    pub fn class(&self) -> &str {
        match self {
            Self::Emulator(manifest) => manifest.class(),
            Self::Platform(manifest) => manifest.class(),
            Self::Scraper(manifest) => manifest.class(),
            Self::Theme(manifest) => manifest.class(),
        }
    }

    pub fn version(&self) -> &str {
        match self {
            Self::Emulator(manifest) => manifest.version(),
            Self::Platform(manifest) => manifest.version(),
            Self::Scraper(manifest) => manifest.version(),
            Self::Theme(manifest) => manifest.version(),
        }
    }

    pub fn api(&self) -> &semver::VersionReq {
        match self {
            Self::Emulator(manifest) => manifest.api(),
            Self::Platform(manifest) => manifest.api(),
            Self::Scraper(manifest) => manifest.api(),
            Self::Theme(manifest) => manifest.api(),
        }
    }
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
    #[error("{}: {message}", dunce::simplified(path).display())]
    Archive { path: PathBuf, message: String },
    #[error("{}: {source}", dunce::simplified(path).display())]
    Unreadable {
        path: PathBuf,
        source: std::io::Error,
    },
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
    #[error("plugin class '{0}' is not known")]
    UnknownClass(String),
    #[error("plugin class '{actual}' does not match expected class '{expected}'")]
    UnexpectedClass {
        expected: &'static str,
        actual: String,
    },
    #[error("plugin module specifier: {0}")]
    ModuleSpecifier(String),
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
