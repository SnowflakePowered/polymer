use deno_ast::{
    EmitOptions, MediaType, ParseParams, TranspileModuleOptions, TranspileOptions, parse_module,
};
use deno_core::{
    ModuleLoadOptions, ModuleLoadReferrer, ModuleLoadResponse, ModuleLoader, ModuleSource,
    ModuleSourceCode, ModuleSpecifier, ModuleType, ResolutionKind, error::ModuleLoaderError,
    resolve_import,
};
use futures_util::FutureExt;
use std::{path::PathBuf, sync::Arc};

mod jsr;

const PLUGIN_SCHEME: &str = "snowglass-pkg";
const PLUGIN_HOST: &str = "plugin";
const RESOURCE_SCHEME: &str = "snowglass-resource";
const PLUGIN_STORAGE_MODULE: &str = "snowglass:pluginStorage";
const CACHE_MODULE: &str = "snowglass:cache";
const DATABASE_MODULE: &str = "snowglass:database";
const JSR_ORIGIN: &str = "https://jsr.io";

/// Module resolver and source transformer bound permanently to one package.
#[derive(Clone, Debug)]
pub struct PluginModuleLoader {
    id: Arc<str>,
    root: Arc<PathBuf>,
    base: ModuleSpecifier,
    jsr_cache: Option<Arc<PathBuf>>,
}

impl PluginModuleLoader {
    pub(crate) fn new(id: &str, root: PathBuf) -> Self {
        let base = ModuleSpecifier::parse(&format!("{PLUGIN_SCHEME}://{PLUGIN_HOST}/{id}/"))
            .expect("validated plugin IDs produce valid module URLs");
        Self {
            id: id.into(),
            root: Arc::new(root),
            base,
            jsr_cache: None,
        }
    }

    pub(crate) fn with_directories(&self, directories: &snowglass_library::Directories) -> Self {
        let mut loader = self.clone();
        loader.jsr_cache = Some(Arc::new(directories.jsr_cache()));
        loader
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    pub fn owns(&self, specifier: &ModuleSpecifier) -> bool {
        specifier.as_str().starts_with(self.base.as_str())
    }

    fn resource_specifier(&self, specifier: &str) -> Result<ModuleSpecifier, ModuleLoaderError> {
        let name = specifier
            .strip_prefix("resources:")
            .expect("resource resolution is only called for resources: specifiers");
        if name.is_empty()
            || name.chars().any(|character| {
                !(character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
            })
        {
            return Err(ModuleLoaderError::type_error(format!(
                "resources: import requires a declared resource name: {specifier}"
            )));
        }
        ModuleSpecifier::parse(&format!(
            "{RESOURCE_SCHEME}://{PLUGIN_HOST}/{}/{name}",
            self.id
        ))
        .map_err(ModuleLoaderError::from_err)
    }

    fn resource_name<'a>(&self, specifier: &'a ModuleSpecifier) -> Option<&'a str> {
        let prefix = format!("{RESOURCE_SCHEME}://{PLUGIN_HOST}/{}/", self.id);
        specifier.as_str().strip_prefix(&prefix)
    }

    pub fn specifier(&self, path: &std::path::Path) -> Result<ModuleSpecifier, ModuleLoaderError> {
        let found = path.canonicalize().map_err(ModuleLoaderError::from_err)?;
        if !found.starts_with(self.root()) {
            return Err(ModuleLoaderError::type_error(format!(
                "plugin module escapes package root: {}",
                found.display()
            )));
        }
        let relative = found.strip_prefix(self.root()).expect("root was checked");
        let relative = relative.to_string_lossy().replace('\\', "/");
        self.base
            .join(&relative)
            .map_err(ModuleLoaderError::from_err)
    }

    pub(crate) fn package_specifier(
        &self,
        path: &std::path::Path,
    ) -> Result<ModuleSpecifier, ModuleLoaderError> {
        if path.as_os_str().is_empty()
            || path.is_absolute()
            || path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(ModuleLoaderError::type_error(format!(
                "plugin main must be a package-relative path: {}",
                path.display()
            )));
        }
        let relative = path.to_string_lossy().replace('\\', "/");
        self.base
            .join(&relative)
            .map_err(ModuleLoaderError::from_err)
    }

    fn resolve_package(&self, specifier: &str) -> Result<ModuleSpecifier, ModuleLoaderError> {
        let package_path = PathBuf::from(
            specifier
                .strip_prefix("pkg:")
                .expect("package resolution is only called for pkg: specifiers")
                .trim_start_matches('/'),
        );
        if package_path.as_os_str().is_empty()
            || package_path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(ModuleLoaderError::type_error(format!(
                "pkg: import must be a package-relative path: {specifier}"
            )));
        }
        let relative = package_path.to_string_lossy().replace('\\', "/");
        self.base
            .join(&relative)
            .map_err(ModuleLoaderError::from_err)
    }

    fn path(&self, specifier: &ModuleSpecifier) -> Result<PathBuf, ModuleLoaderError> {
        let relative = specifier
            .as_str()
            .strip_prefix(self.base.as_str())
            .ok_or_else(|| {
                ModuleLoaderError::type_error(format!(
                    "module {specifier} does not belong to plugin '{}'",
                    self.id
                ))
            })?;
        let file_base = ModuleSpecifier::from_directory_path(self.root()).map_err(|()| {
            ModuleLoaderError::type_error(format!(
                "cannot convert plugin root to a file URL: {}",
                self.root.display()
            ))
        })?;
        let file = file_base
            .join(relative)
            .map_err(ModuleLoaderError::from_err)?;
        let path = file.to_file_path().map_err(|()| {
            ModuleLoaderError::type_error(format!("plugin module is not a file path: {file}"))
        })?;
        let found = path.canonicalize().map_err(ModuleLoaderError::from_err)?;
        if !found.starts_with(self.root()) {
            return Err(ModuleLoaderError::type_error(format!(
                "plugin module escapes package root: {}",
                found.display()
            )));
        }
        Ok(found)
    }

    fn load_source(
        &self,
        specifier: &ModuleSpecifier,
        module_type: ModuleType,
    ) -> Result<ModuleSource, ModuleLoaderError> {
        let path = self.path(specifier)?;
        let code = std::fs::read(&path).map_err(ModuleLoaderError::from_err)?;
        let code = self.transform_source(specifier, code)?;
        Ok(ModuleSource::new(
            module_type,
            ModuleSourceCode::Bytes(code.into_boxed_slice().into()),
            specifier,
            None,
        ))
    }

    fn transform_source(
        &self,
        specifier: &ModuleSpecifier,
        code: Vec<u8>,
    ) -> Result<Vec<u8>, ModuleLoaderError> {
        match specifier
            .path()
            .rsplit_once('.')
            .map(|(_, extension)| extension)
        {
            Some("ts" | "tsx" | "mts" | "cts") => {
                let text = String::from_utf8(code)
                    .map_err(|error| ModuleLoaderError::type_error(error.to_string()))?;
                let parsed = parse_module(ParseParams {
                    specifier: specifier.clone(),
                    text: text.into(),
                    media_type: MediaType::from_specifier(specifier),
                    capture_tokens: false,
                    scope_analysis: false,
                    maybe_syntax: None,
                })
                .map_err(|error| ModuleLoaderError::type_error(error.to_string()))?;
                Ok(parsed
                    .transpile(
                        &TranspileOptions::default(),
                        &TranspileModuleOptions::default(),
                        &EmitOptions::default(),
                    )
                    .map_err(|error| ModuleLoaderError::type_error(error.to_string()))?
                    .into_source()
                    .text
                    .into_bytes())
            }
            Some("toml") => {
                let text = String::from_utf8(code)
                    .map_err(|error| ModuleLoaderError::type_error(error.to_string()))?;
                let value = text
                    .parse::<toml::Table>()
                    .map_err(|error| ModuleLoaderError::type_error(error.to_string()))?;
                Ok(include_str!("js/04_toml.js")
                    .replace(
                        "__SNOWGLASS_TOML__",
                        &serde_json::to_string(&value).unwrap(),
                    )
                    .into_bytes())
            }
            _ => Ok(code),
        }
    }
}

impl ModuleLoader for PluginModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        _kind: ResolutionKind,
    ) -> Result<ModuleSpecifier, ModuleLoaderError> {
        let resolved = if matches!(
            specifier,
            PLUGIN_STORAGE_MODULE | CACHE_MODULE | DATABASE_MODULE
        ) {
            ModuleSpecifier::parse(specifier).map_err(ModuleLoaderError::from_err)?
        } else if specifier.starts_with("jsr:") {
            self.resolve_jsr(specifier)?
        } else if specifier.starts_with("resources:") {
            self.resource_specifier(specifier)?
        } else if specifier.starts_with("pkg:") {
            self.resolve_package(specifier)?
        } else {
            resolve_import(specifier, referrer).map_err(ModuleLoaderError::from_err)?
        };
        if resolved.origin().ascii_serialization() == JSR_ORIGIN
            && !specifier.starts_with("jsr:")
            && ModuleSpecifier::parse(referrer)
                .map(|value| value.origin().ascii_serialization() != JSR_ORIGIN)
                .unwrap_or(true)
        {
            return Err(ModuleLoaderError::type_error(
                "plugins must import JSR packages through a jsr: specifier",
            ));
        }
        if !matches!(
            resolved.as_str(),
            PLUGIN_STORAGE_MODULE | CACHE_MODULE | DATABASE_MODULE
        ) && !self.owns(&resolved)
            && self.resource_name(&resolved).is_none()
            && resolved.origin().ascii_serialization() != JSR_ORIGIN
        {
            return Err(ModuleLoaderError::type_error(format!(
                "plugin '{}' import escapes its package: {resolved}",
                self.id
            )));
        }
        Ok(resolved)
    }

    fn load(
        &self,
        module_specifier: &ModuleSpecifier,
        _maybe_referrer: Option<&ModuleLoadReferrer>,
        options: ModuleLoadOptions,
    ) -> ModuleLoadResponse {
        if module_specifier.origin().ascii_serialization() == JSR_ORIGIN {
            let loader = self.clone();
            let specifier = module_specifier.clone();
            let module_type = requested_module_type(&options);
            return ModuleLoadResponse::Async(
                async move { loader.load_jsr(specifier, module_type).await }.boxed_local(),
            );
        }
        let source = if module_specifier.as_str() == PLUGIN_STORAGE_MODULE {
            Ok(ModuleSource::new(
                ModuleType::JavaScript,
                ModuleSourceCode::String(include_str!("js/00_plugin_storage.js").to_owned().into()),
                module_specifier,
                None,
            ))
        } else if module_specifier.as_str() == CACHE_MODULE {
            Ok(ModuleSource::new(
                ModuleType::JavaScript,
                ModuleSourceCode::String(include_str!("js/01_cache.js").to_owned().into()),
                module_specifier,
                None,
            ))
        } else if module_specifier.as_str() == DATABASE_MODULE {
            Ok(ModuleSource::new(
                ModuleType::JavaScript,
                ModuleSourceCode::String(include_str!("js/02_database.js").to_owned().into()),
                module_specifier,
                None,
            ))
        } else if let Some(name) = self.resource_name(module_specifier) {
            let code = include_str!("js/03_resource.js").replace(
                "__SNOWGLASS_RESOURCE__",
                &serde_json::to_string(name).expect("resource name is JSON-compatible"),
            );
            Ok(ModuleSource::new(
                ModuleType::JavaScript,
                ModuleSourceCode::String(code.into()),
                module_specifier,
                None,
            ))
        } else {
            self.load_source(module_specifier, requested_module_type(&options))
        };
        ModuleLoadResponse::Sync(source)
    }
}

fn requested_module_type(options: &ModuleLoadOptions) -> ModuleType {
    match &options.requested_module_type {
        deno_core::RequestedModuleType::Json => ModuleType::Json,
        deno_core::RequestedModuleType::Text => ModuleType::Text,
        deno_core::RequestedModuleType::Bytes => ModuleType::Bytes,
        deno_core::RequestedModuleType::Other(module_type) => {
            ModuleType::Other(module_type.clone())
        }
        deno_core::RequestedModuleType::None => ModuleType::JavaScript,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_only_shared_api_imports_are_erased_before_module_resolution() {
        let root = tempfile::tempdir().unwrap();
        let loader = PluginModuleLoader::new("test", root.path().to_owned());
        let specifier = loader.base.join("plugin.ts").unwrap();
        let output = loader
            .transform_source(
                &specifier,
                br#"import type { Filesystem } from "../plugin-api.ts";
                    export function setup(filesystem: Filesystem) { return Boolean(filesystem); }"#
                    .to_vec(),
            )
            .unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(!output.contains("plugin-api"));
        assert!(output.contains("export function setup"));
    }

    #[test]
    fn jsr_resolution_preserves_version_requirements_and_exports() {
        let root = tempfile::tempdir().unwrap();
        let loader = PluginModuleLoader::new("test", root.path().to_owned());
        let resolved = loader
            .resolve_jsr("jsr:@std/xml@^0.2/parse-stream")
            .unwrap();
        let query = resolved
            .query_pairs()
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(query.get("package").unwrap(), "@std/xml");
        assert_eq!(query.get("version").unwrap(), "^0.2");
        assert_eq!(query.get("export").unwrap(), "./parse-stream");
    }

    #[tokio::test]
    async fn jsr_reads_the_cache_owned_by_library_directories() {
        let root = tempfile::tempdir().unwrap();
        let directories = snowglass_library::Directories::rooted(root.path(), root.path());
        let loader =
            PluginModuleLoader::new("test", root.path().to_owned()).with_directories(&directories);
        let url = "https://jsr.io/@std/example/1.0.0/mod.ts";
        let cached = directories
            .jsr_cache()
            .join(blake3::hash(url.as_bytes()).to_hex().to_string());
        std::fs::create_dir_all(cached.parent().unwrap()).unwrap();
        std::fs::write(&cached, b"export const cached = true;").unwrap();

        assert_eq!(
            loader.cached_get(url).await.unwrap(),
            b"export const cached = true;"
        );
    }

    #[test]
    fn bundled_emulator_plugins_transpile_against_the_shared_api() {
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        for name in [
            "cemu",
            "dolphin",
            "duckstation",
            "flycast",
            "mgba",
            "pcsx2",
            "retroarch",
        ] {
            let root = workspace.join("plugins").join(name).canonicalize().unwrap();
            let source = root.join(format!("{name}.ts"));
            let loader = PluginModuleLoader::new(name, root.clone());
            let specifier = loader.specifier(&source).unwrap();
            let output = loader
                .transform_source(&specifier, std::fs::read(source).unwrap())
                .unwrap();
            let output = String::from_utf8(output).unwrap();
            assert!(!output.contains("../plugin-api.ts"));
            assert!(!output.contains("../hotkeys.ts"));

            let hotkeys = root.join("hotkeys.ts");
            if hotkeys.exists() {
                let specifier = loader.specifier(&hotkeys).unwrap();
                let output = loader
                    .transform_source(&specifier, std::fs::read(hotkeys).unwrap())
                    .unwrap();
                assert!(
                    !String::from_utf8(output)
                        .unwrap()
                        .contains("../plugin-api.ts")
                );
            }
        }
    }

    #[test]
    fn bundled_launchbox_scraper_transpiles_against_the_shared_api() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../plugins/launchbox")
            .canonicalize()
            .unwrap();
        let source = root.join("launchbox.ts");
        let loader = PluginModuleLoader::new("launchbox", root);
        let specifier = loader.specifier(&source).unwrap();
        let output = loader
            .transform_source(&specifier, std::fs::read(source).unwrap())
            .unwrap();
        assert!(
            !String::from_utf8(output)
                .unwrap()
                .contains("../plugin-api.ts")
        );
    }
}
