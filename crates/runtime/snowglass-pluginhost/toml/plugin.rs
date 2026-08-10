use serde::{Deserialize, de::DeserializeOwned};
use std::{
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
    #[serde(default)]
    config: toml::Table,
}

/// Generic data shared by every loaded plugin package. `B` is the root block
/// belonging to the plugin class, such as `[emulator]`.
pub struct PluginPackage<B> {
    root: PathBuf,
    plugin: PluginBlock,
    block: B,
}

impl<B: DeserializeOwned> PluginPackage<B> {
    pub fn load(root: impl AsRef<Path>) -> Result<Self, PackageError> {
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
        let class = plugin.class.clone();
        let block: B = manifest
            .remove(&class)
            .ok_or_else(|| PackageError::MissingBlock(class.clone()))?
            .try_into()?;
        Ok(Self {
            root,
            plugin,
            block,
        })
    }
}

impl<B> PluginPackage<B> {
    pub fn root(&self) -> &Path {
        &self.root
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

    pub fn config(&self) -> &toml::Table {
        &self.plugin.config
    }

    pub fn block(&self) -> &B {
        &self.block
    }

    pub fn module_loader(&self) -> crate::PluginModuleLoader {
        crate::PluginModuleLoader::new(self.id(), self.root.clone())
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
    #[error(
        "layout '{layout}' does not identify one next port on '{console}' (candidates: {candidates:?})"
    )]
    AmbiguousLayout {
        console: String,
        layout: String,
        candidates: Vec<String>,
    },
}
