use crate::{PluginHost, PluginInstance};
use deno_ast::{
    EmitOptions, MediaType, ParseParams, TranspileModuleOptions, TranspileOptions, parse_module,
};
use deno_core::{
    ModuleLoadOptions, ModuleLoadReferrer, ModuleLoadResponse, ModuleLoader, ModuleSource,
    ModuleSourceCode, ModuleSpecifier, ModuleType, ResolutionKind, error::ModuleLoaderError,
    resolve_import,
};
use std::{path::PathBuf, rc::Rc, sync::Arc};

const PLUGIN_SCHEME: &str = "snowglass-pkg";
const PLUGIN_HOST: &str = "plugin";

/// Module resolver and source transformer bound permanently to one package.
#[derive(Clone, Debug)]
pub struct PluginModuleLoader {
    id: Arc<str>,
    root: Arc<PathBuf>,
    base: ModuleSpecifier,
}

impl PluginModuleLoader {
    pub(crate) fn new(id: &str, root: PathBuf) -> Self {
        let base = ModuleSpecifier::parse(&format!("{PLUGIN_SCHEME}://{PLUGIN_HOST}/{id}/"))
            .expect("validated plugin IDs produce valid module URLs");
        Self {
            id: id.into(),
            root: Arc::new(root),
            base,
        }
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
                Ok(
                    format!("export default {};", serde_json::to_string(&value).unwrap())
                        .into_bytes(),
                )
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
        let resolved = if specifier.starts_with("pkg:") {
            self.resolve_package(specifier)?
        } else {
            resolve_import(specifier, referrer).map_err(ModuleLoaderError::from_err)?
        };
        if !self.owns(&resolved) {
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
        ModuleLoadResponse::Sync(
            self.load_source(module_specifier, requested_module_type(&options)),
        )
    }
}

/// The single Deno loader installed by the embedding application. Plugin
/// modules are routed by their canonical referrer; everything else is handed
/// to the application's loader.
#[derive(Clone)]
pub struct PluginModuleRouter {
    plugins: PluginHost,
    fallback: Rc<dyn ModuleLoader>,
}

impl PluginModuleRouter {
    pub fn new(plugins: PluginHost, fallback: Rc<dyn ModuleLoader>) -> Self {
        Self { plugins, fallback }
    }

    fn plugin(&self, specifier: &ModuleSpecifier) -> Option<Arc<dyn PluginInstance>> {
        let id = plugin_id(specifier)?;
        self.plugins.plugin::<dyn PluginInstance>(id)
    }
}

impl ModuleLoader for PluginModuleRouter {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        kind: ResolutionKind,
    ) -> Result<ModuleSpecifier, ModuleLoaderError> {
        let plugin_referrer = ModuleSpecifier::parse(referrer)
            .ok()
            .filter(|referrer| plugin_id(referrer).is_some());
        if specifier.starts_with("pkg:") || plugin_referrer.is_some() {
            let referrer = plugin_referrer.ok_or_else(|| {
                ModuleLoaderError::type_error(format!(
                    "pkg: import has no plugin package referrer: {referrer}"
                ))
            })?;
            let plugin = self.plugin(&referrer).ok_or_else(|| {
                ModuleLoaderError::type_error(format!(
                    "plugin import has no registered package referrer: {referrer}"
                ))
            })?;
            plugin
                .module_loader()
                .resolve(specifier, referrer.as_str(), kind)
        } else {
            self.fallback.resolve(specifier, referrer, kind)
        }
    }

    fn load(
        &self,
        module_specifier: &ModuleSpecifier,
        maybe_referrer: Option<&ModuleLoadReferrer>,
        options: ModuleLoadOptions,
    ) -> ModuleLoadResponse {
        if let Some(plugin) = self.plugin(module_specifier) {
            plugin
                .module_loader()
                .load(module_specifier, maybe_referrer, options)
        } else {
            self.fallback
                .load(module_specifier, maybe_referrer, options)
        }
    }
}

fn plugin_id(specifier: &ModuleSpecifier) -> Option<&str> {
    if specifier.scheme() != PLUGIN_SCHEME || specifier.host_str() != Some(PLUGIN_HOST) {
        return None;
    }
    specifier
        .path_segments()?
        .next()
        .filter(|id| !id.is_empty())
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
