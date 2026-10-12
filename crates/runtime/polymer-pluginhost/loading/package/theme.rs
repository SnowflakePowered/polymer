use super::plugin::{PackageError, PluginClass, PluginPackage};
use crate::theme::ThemePluginInstance;
use deno_core::ModuleSpecifier;
use serde::Deserialize;
use std::path::{Component, Path, PathBuf};

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
    pub url: ModuleSpecifier,
}

pub type ThemePlugin = PluginPackage<ThemeBlock>;

impl PluginClass for ThemeBlock {
    const CLASS: &'static str = "theme";

    fn prepare(package: &mut PluginPackage<Self>) -> Result<(), PackageError> {
        let debug = package
            .block()
            .manifest_debug
            .as_ref()
            .map(|debug| {
                if debug.serve.trim().is_empty() {
                    return Err(PackageError::InvalidThemeServe);
                }
                let url = ModuleSpecifier::parse(&debug.url)
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
        ThemeBlock::validate(package.block().root.as_ref())?;
        ThemeBlock::validate(package.block().assets.as_ref())?;
        package.block_mut().debug = debug;
        Ok(())
    }
}

impl PluginPackage<ThemeBlock> {
    pub fn debug(&self) -> Option<&ThemeDebug> {
        self.block().debug.as_ref()
    }

    pub fn theme_root(&self) -> Option<&Path> {
        self.block().root.as_deref()
    }

    pub fn assets(&self) -> Option<&Path> {
        self.block().assets.as_deref()
    }
}

impl ThemePluginInstance for PluginPackage<ThemeBlock> {
    fn root(&self) -> &Path {
        self.root()
    }

    fn main(&self) -> &Path {
        self.main()
    }

    fn debug(&self) -> Option<&ThemeDebug> {
        self.debug()
    }

    fn theme_root(&self) -> Option<&Path> {
        self.theme_root()
    }

    fn assets(&self) -> Option<&Path> {
        self.assets()
    }
}
