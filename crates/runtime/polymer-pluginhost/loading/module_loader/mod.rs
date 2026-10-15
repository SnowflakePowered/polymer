use deno_ast::{
    EmitOptions, MediaType, ParseParams, TranspileModuleOptions, TranspileOptions, parse_module,
};
use deno_core::{
    ModuleLoadOptions, ModuleLoadReferrer, ModuleLoadResponse, ModuleLoader, ModuleSource,
    ModuleSourceCode, ModuleSpecifier, ModuleType, ResolutionKind, error::ModuleLoaderError,
    resolve_import,
};
use dashmap::DashMap;
use futures_util::FutureExt;
use polymer_pluginmanifest::{PackageEntry, PackageSource};
use std::{path::PathBuf, sync::Arc};

mod jsr;

const PLUGIN_SCHEME: &str = "pkg";
const PLUGIN_HOST: &str = "plugin";
const SYS_MODULE: &str = "polymer:sys";
const API_MODULE: &str = "polymer:api";
const UI_AUTOMATION_MODULE: &str = "polymer:uiautomation";
const CONSOLESTORAGE_MODULE: &str = "polymer:consolestorage";
const DATABASE_MODULE: &str = "polymer:database";
const DOWNLOAD_MODULE: &str = "polymer:download";
const JOBS_MODULE: &str = "polymer:jobs";
const UNARCHIVE_MODULE: &str = "polymer:unarchive";
const PLUGIN_MODULE: &str = "polymer:plugin";
const JSR_ORIGIN: &str = "https://jsr.io";

/// Module resolver and source transformer bound permanently to one package.
#[derive(Clone, Debug)]
pub struct PluginModuleLoader {
    id: Arc<str>,
    source: PackageSource,
    base: ModuleSpecifier,
    jsr_cache: Option<Arc<PathBuf>>,
    network_hosts: Vec<String>,
    insecure_hosts: Arc<[String]>,
    jsr_overrides: Arc<DashMap<String, PathBuf>>,
}

impl PluginModuleLoader {
    pub(crate) fn new(id: &str, source: PackageSource) -> Self {
        let base = ModuleSpecifier::parse(&format!("{PLUGIN_SCHEME}://{PLUGIN_HOST}/{id}/"))
            .expect("validated plugin IDs produce valid module URLs");
        Self {
            id: id.into(),
            source,
            base,
            jsr_cache: None,
            network_hosts: Vec::new(),
            insecure_hosts: Arc::new([]),
            jsr_overrides: Arc::default(),
        }
    }

    pub(crate) fn with_directories(&self, directories: &polymer_library::Directories) -> Self {
        let mut loader = self.clone();
        loader.jsr_cache = Some(Arc::new(directories.jsr_cache()));
        loader
    }

    /// Resolves the given JSR packages from local directories instead of jsr.io.
    pub(crate) fn with_jsr_overrides(mut self, overrides: Arc<DashMap<String, PathBuf>>) -> Self {
        self.jsr_overrides = overrides;
        self
    }

    fn overridden(&self, specifier: &ModuleSpecifier) -> bool {
        specifier.scheme() == "file"
            && specifier.to_file_path().is_ok_and(|path| {
                self.jsr_overrides
                    .iter()
                    .any(|entry| path.starts_with(entry.value()))
            })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn with_network_hosts(mut self, hosts: Vec<String>) -> Self {
        self.network_hosts = hosts;
        self
    }

    pub(crate) fn network_hosts(&self) -> &[String] {
        &self.network_hosts
    }

    pub(crate) fn with_insecure_hosts(mut self, hosts: Vec<String>) -> Self {
        self.insecure_hosts = hosts.into();
        self
    }

    /// Allowed hosts whose certificates the plugin's requests do not verify.
    pub(crate) fn insecure_hosts(&self) -> &Arc<[String]> {
        &self.insecure_hosts
    }

    pub fn source(&self) -> &PackageSource {
        &self.source
    }

    /// The module URL of a validated package-relative path.
    pub(crate) fn package_specifier(&self, relative: &str) -> Result<ModuleSpecifier, ModuleLoaderError> {
        self.base.join(relative).map_err(ModuleLoaderError::from_err)
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

    /// The package-relative path a module URL names.
    fn relative(&self, specifier: &ModuleSpecifier) -> Result<String, ModuleLoaderError> {
        let mut file = specifier.clone();
        file.set_query(None);
        file.set_fragment(None);
        let relative = file
            .as_str()
            .strip_prefix(self.base.as_str())
            .ok_or_else(|| {
                ModuleLoaderError::type_error(format!(
                    "module {specifier} does not belong to plugin '{}'",
                    self.id
                ))
            })?;
        let decoded = percent_encoding::percent_decode_str(relative)
            .decode_utf8()
            .map_err(|error| ModuleLoaderError::type_error(error.to_string()))?;
        PackageSource::relative(std::path::Path::new(decoded.as_ref()))
            .map_err(|error| ModuleLoaderError::type_error(error.to_string()))
    }

    fn load_source(
        &self,
        specifier: &ModuleSpecifier,
        module_type: ModuleType,
    ) -> Result<ModuleSource, ModuleLoaderError> {
        let relative = self.relative(specifier)?;
        let entry = self.source.entry(&relative).ok_or_else(|| {
            ModuleLoaderError::type_error(format!("plugin module {specifier} does not exist"))
        })?;
        let extension = relative
            .rsplit_once('.')
            .map(|(_, extension)| extension.to_ascii_lowercase())
            .unwrap_or_default();
        if entry == PackageEntry::Directory
            || extension == "asar"
            || (module_type == ModuleType::JavaScript
                && !matches!(
                    extension.as_str(),
                    "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts" | "json" | "toml"
                ))
        {
            if module_type != ModuleType::JavaScript {
                return Err(ModuleLoaderError::type_error(
                    "package directories and archives must be imported as JavaScript resources",
                ));
            }
            let relative = specifier
                .as_str()
                .strip_prefix(self.base.as_str())
                .expect("package ownership was checked");
            let source = format!("pkg:{relative}");
            let code = include_str!("js/03_resource.js").replace(
                "__POLYMER_RESOURCE__",
                &serde_json::to_string(&source).expect("package URL is JSON-compatible"),
            );
            return Ok(ModuleSource::new(
                ModuleType::JavaScript,
                ModuleSourceCode::String(code.into()),
                specifier,
                None,
            ));
        }
        let code = self
            .source
            .read(&relative)
            .map_err(|error| ModuleLoaderError::type_error(error.to_string()))?
            .into_owned();
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
                    .replace("__POLYMER_TOML__", &serde_json::to_string(&value).unwrap())
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
        if matches!(
            specifier,
            API_MODULE
                | SYS_MODULE
                | UI_AUTOMATION_MODULE
                | CONSOLESTORAGE_MODULE
                | DATABASE_MODULE
                | DOWNLOAD_MODULE
                | JOBS_MODULE
                | UNARCHIVE_MODULE
                | PLUGIN_MODULE
        ) {
            return ModuleSpecifier::parse(specifier).map_err(ModuleLoaderError::from_err);
        }
        if specifier.starts_with("jsr:") {
            return self.resolve_jsr(specifier);
        }
        if specifier.starts_with(self.base.as_str()) {
            return ModuleSpecifier::parse(specifier).map_err(ModuleLoaderError::from_err);
        }
        if specifier.starts_with("pkg:") && !specifier.starts_with("pkg://") {
            return self.resolve_package(specifier);
        }
        if let Ok(referrer) = ModuleSpecifier::parse(referrer)
            && self.overridden(&referrer)
        {
            let resolved =
                resolve_import(specifier, referrer.as_str()).map_err(ModuleLoaderError::from_err)?;
            if self.overridden(&resolved) {
                return Ok(resolved);
            }
        }
        // Registry dependencies resolve their own module graph, never local plugin paths.
        if ModuleSpecifier::parse(referrer)
            .is_ok_and(|url| url.origin().ascii_serialization() == JSR_ORIGIN)
        {
            let resolved =
                resolve_import(specifier, referrer).map_err(ModuleLoaderError::from_err)?;
            if resolved.origin().ascii_serialization() == JSR_ORIGIN {
                return Ok(resolved);
            }
        }
        Err(ModuleLoaderError::type_error(format!(
            "unsupported plugin import '{specifier}': use pkg:, polymer:, or jsr:"
        )))
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
        if self.overridden(module_specifier) {
            let source = module_specifier
                .to_file_path()
                .map_err(|()| {
                    ModuleLoaderError::type_error(format!("not a file module: {module_specifier}"))
                })
                .and_then(|path| std::fs::read(path).map_err(ModuleLoaderError::from_err))
                .and_then(|code| self.transform_source(module_specifier, code))
                .map(|code| {
                    ModuleSource::new(
                        requested_module_type(&options),
                        ModuleSourceCode::Bytes(code.into_boxed_slice().into()),
                        module_specifier,
                        None,
                    )
                });
            return ModuleLoadResponse::Sync(source);
        }
        let source = if module_specifier.as_str() == SYS_MODULE {
            let os = match std::env::consts::OS {
                "windows" => "win",
                os => os,
            };
            let architecture = match std::env::consts::ARCH {
                "x86_64" => "x64",
                "aarch64" => "arm64",
                architecture => architecture,
            };
            #[cfg(windows)]
            let graphics = polymer_platform_windows::GraphicsSupport::available();
            #[cfg(target_os = "macos")]
            let graphics = polymer_platform_macos::GraphicsSupport::available();
            #[cfg(not(any(windows, target_os = "macos")))]
            let graphics: &[polymer_protocol::GraphicsHook] = &[];
            serde_json::to_string(graphics)
                .map(|graphics| {
                    ModuleSource::new(
                        ModuleType::JavaScript,
                        ModuleSourceCode::String(
                            format!(
                                "export const osIdentifier = '{os}-{architecture}';
export const graphicsApis = Object.freeze({graphics});"
                            )
                            .into(),
                        ),
                        module_specifier,
                        None,
                    )
                })
                .map_err(ModuleLoaderError::from_err)
        } else if module_specifier.as_str() == API_MODULE {
            Ok(ModuleSource::new(
                ModuleType::JavaScript,
                ModuleSourceCode::String("export {};".to_owned().into()),
                module_specifier,
                None,
            ))
        } else if module_specifier.as_str() == UI_AUTOMATION_MODULE {
            Ok(ModuleSource::new(
                ModuleType::JavaScript,
                ModuleSourceCode::String(include_str!("js/05_ui_automation.js").to_owned().into()),
                module_specifier,
                None,
            ))
        } else if module_specifier.as_str() == CONSOLESTORAGE_MODULE {
            Ok(ModuleSource::new(
                ModuleType::JavaScript,
                ModuleSourceCode::String(include_str!("js/07_consolestorage.js").to_owned().into()),
                module_specifier,
                None,
            ))
        } else if module_specifier.as_str() == DOWNLOAD_MODULE {
            Ok(ModuleSource::new(
                ModuleType::JavaScript,
                ModuleSourceCode::String(include_str!("js/08_download.js").to_owned().into()),
                module_specifier,
                None,
            ))
        } else if module_specifier.as_str() == JOBS_MODULE {
            Ok(ModuleSource::new(
                ModuleType::JavaScript,
                ModuleSourceCode::String(include_str!("js/09_jobs.js").to_owned().into()),
                module_specifier,
                None,
            ))
        } else if module_specifier.as_str() == UNARCHIVE_MODULE {
            Ok(ModuleSource::new(
                ModuleType::JavaScript,
                ModuleSourceCode::String(include_str!("js/10_unarchive.js").to_owned().into()),
                module_specifier,
                None,
            ))
        } else if module_specifier.as_str() == PLUGIN_MODULE {
            Ok(ModuleSource::new(
                ModuleType::JavaScript,
                ModuleSourceCode::String(include_str!("js/11_plugin.js").to_owned().into()),
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
