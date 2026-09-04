use super::plugin::{PackageError, PluginClass, PluginPackage};
use crate::ThemePluginInstance;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PluginInstance;
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
main = "{main}"

[theme]
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
        assert_eq!(
            crate::PluginInstance::entrypoint(&theme).as_str(),
            "snowglass-pkg://plugin/chipotle/dist/index.html"
        );
    }

    #[test]
    fn rejects_escaping_main_and_non_http_debug_url() {
        let escaping = TempDir::new().unwrap();
        write_theme(escaping.path(), "../index.html", "");
        assert!(matches!(
            ThemePlugin::load(escaping.path()),
            Err(PackageError::InvalidPluginMain(_))
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
