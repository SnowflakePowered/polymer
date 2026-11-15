use serde::Deserialize;
use std::path::{Component, Path, PathBuf};

use crate::{PackageError, PluginClass, PluginManifest};

#[derive(Debug, Deserialize)]
pub struct ThemeBlock {
    #[serde(default, rename = "debug")]
    manifest_debug: Option<ThemeDebugBlock>,
    #[serde(skip)]
    debug: Option<ThemeDebug>,
    #[serde(default)]
    root: Option<PathBuf>,
    #[serde(default)]
    assets: Option<PathBuf>,
    #[serde(default)]
    chrome: ThemeChrome,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
struct ThemeChrome {
    titlebar: bool,
}

impl Default for ThemeChrome {
    fn default() -> Self {
        Self { titlebar: true }
    }
}

impl ThemeBlock {
    fn validate(path: Option<&PathBuf>) -> Result<(), PackageError> {
        let Some(path) = path else {
            return Ok(());
        };
        let relative = path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir));
        if relative && path.components().next().is_some() {
            Ok(())
        } else {
            Err(PackageError::EscapesPackage(path.clone()))
        }
    }
}

#[derive(Debug, Deserialize)]
struct ThemeDebugBlock {
    serve: String,
    url: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThemeDebug {
    pub serve: String,
    pub url: url::Url,
}

impl PluginClass for ThemeBlock {
    const CLASS: &'static str = "theme";

    fn prepare(manifest: &mut PluginManifest<Self>) -> Result<(), PackageError> {
        let debug = manifest
            .block()
            .manifest_debug
            .as_ref()
            .map(|debug| {
                if debug.serve.trim().is_empty() {
                    return Err(PackageError::InvalidThemeServe);
                }
                let url = url::Url::parse(&debug.url)
                    .map_err(|error| PackageError::InvalidThemeDebugUrl(error.to_string()))?;
                if !matches!(url.scheme(), "http" | "https") {
                    return Err(PackageError::InvalidThemeDebugUrl(debug.url.clone()));
                }
                Ok(ThemeDebug {
                    serve: debug.serve.clone(),
                    url,
                })
            })
            .transpose()?;
        ThemeBlock::validate(manifest.block().root.as_ref())?;
        ThemeBlock::validate(manifest.block().assets.as_ref())?;
        manifest.block_mut().debug = debug;
        Ok(())
    }
}

impl PluginManifest<ThemeBlock> {
    pub fn debug(&self) -> Option<&ThemeDebug> {
        self.block().debug.as_ref()
    }

    /// The directory the theme is served from, relative to the plugin root.
    pub fn theme_root(&self) -> Option<&Path> {
        self.block().root.as_deref()
    }

    pub fn assets(&self) -> Option<&Path> {
        self.block().assets.as_deref()
    }

    pub fn titlebar(&self) -> bool {
        self.block().chrome.titlebar
    }
}
