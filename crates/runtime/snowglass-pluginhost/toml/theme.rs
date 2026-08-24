use super::plugin::{PackageError, PluginPackage};
use crate::{PluginInstance, PluginModuleLoader, ThemePluginInstance};
use deno_core::ModuleSpecifier;
use serde::Deserialize;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Deserialize)]
struct ThemeBlock {
    main: PathBuf,
    #[serde(default)]
    debug: Option<ThemeDebugBlock>,
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

/// A document theme packaged with its production frontend and optional
/// development-server metadata.
pub struct ThemePlugin {
    package: PluginPackage<ThemeBlock>,
    module_loader: PluginModuleLoader,
    main: PathBuf,
    debug: Option<ThemeDebug>,
}

impl ThemePlugin {
    pub fn load(root: impl AsRef<Path>) -> Result<Self, PackageError> {
        let package = PluginPackage::<ThemeBlock>::load(root)?;
        if package.class() != "theme" {
            return Err(PackageError::UnexpectedClass {
                expected: "theme",
                actual: package.class().to_owned(),
            });
        }
        let theme = package.block();
        validate_relative_main(&theme.main)?;
        let main = package.root().join(&theme.main);
        let debug = theme
            .debug
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
        let module_loader = package.module_loader();
        Ok(Self {
            package,
            module_loader,
            main,
            debug,
        })
    }

    pub fn root(&self) -> &Path {
        self.package.root()
    }

    pub fn main(&self) -> &Path {
        &self.main
    }

    pub fn debug(&self) -> Option<&ThemeDebug> {
        self.debug.as_ref()
    }
}

fn validate_relative_main(path: &Path) -> Result<(), PackageError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(PackageError::InvalidThemeMain(path.to_owned()));
    }
    Ok(())
}

impl PluginInstance for ThemePlugin {
    fn id(&self) -> &str {
        self.package.id()
    }

    fn version(&self) -> &str {
        self.package.version()
    }

    fn class(&self) -> &str {
        self.package.class()
    }

    fn module_loader(&self) -> &PluginModuleLoader {
        &self.module_loader
    }
}

impl ThemePluginInstance for ThemePlugin {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_theme(root: &Path, main: &str, debug: &str) {
        fs::write(
            root.join("plugin.toml"),
            format!(
                r#"[plugin]
id = "chipotle"
version = "0.1.0"
class = "theme"

[theme]
main = "{main}"
{debug}
"#,
            ),
        )
        .unwrap();
    }

    #[test]
    fn loads_debug_theme_without_built_document() {
        let package = TempDir::new().unwrap();
        write_theme(
            package.path(),
            "dist/index.html",
            r#"
[theme.debug]
serve = "vite --host 127.0.0.1 --port 5173"
url = "http://127.0.0.1:5173"
"#,
        );
        let theme = ThemePlugin::load(package.path()).unwrap();
        assert_eq!(theme.id(), "chipotle");
        assert_eq!(
            theme.main(),
            package
                .path()
                .canonicalize()
                .unwrap()
                .join("dist/index.html")
        );
        assert_eq!(
            theme.debug().unwrap().url.as_str(),
            "http://127.0.0.1:5173/"
        );
    }

    #[test]
    fn rejects_escaping_main_and_non_http_debug_url() {
        let escaping = TempDir::new().unwrap();
        write_theme(escaping.path(), "../index.html", "");
        assert!(matches!(
            ThemePlugin::load(escaping.path()),
            Err(PackageError::InvalidThemeMain(_))
        ));

        let invalid_url = TempDir::new().unwrap();
        write_theme(
            invalid_url.path(),
            "dist/index.html",
            r#"
[theme.debug]
serve = "vite"
url = "file:///tmp/index.html"
"#,
        );
        assert!(matches!(
            ThemePlugin::load(invalid_url.path()),
            Err(PackageError::InvalidThemeDebugUrl(_))
        ));
    }
}
