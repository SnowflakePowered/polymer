use super::plugin::{PackageError, PluginClass, PluginPackage};
use crate::theme::ThemePluginInstance;
use deno_core::ModuleSpecifier;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct ThemeBlock {
    #[serde(default, rename = "debug")]
    manifest_debug: Option<ThemeDebugBlock>,
    #[serde(skip)]
    debug: Option<ThemeDebug>,
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
        package.block_mut().debug = debug;
        Ok(())
    }
}

impl PluginPackage<ThemeBlock> {
    pub fn debug(&self) -> Option<&ThemeDebug> {
        self.block().debug.as_ref()
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
}
