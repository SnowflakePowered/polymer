use polymer_pluginmanifest::{PackageError, PluginCache, PluginConfig, PluginManifest};
use std::ops::Deref;

/// A validated plugin manifest with what executing the plugin needs: its module loader and
/// entry point.
pub struct PluginPackage<B> {
    manifest: PluginManifest<B>,
    module_loader: crate::loading::PluginModuleLoader,
    entrypoint: deno_core::ModuleSpecifier,
}

impl<B> PluginPackage<B> {
    pub fn new(manifest: PluginManifest<B>) -> Result<Self, PackageError> {
        polymer_deno::Runtime::network_permissions(manifest.allowed_hosts())
            .map_err(|error| PackageError::ModuleSpecifier(error.to_string()))?;
        let module_loader = crate::loading::PluginModuleLoader::new(manifest.id(), manifest.source().clone())
            .with_network_hosts(manifest.allowed_hosts().to_vec())
            .with_insecure_hosts(manifest.insecure_hosts().to_vec());
        let entrypoint = module_loader
            .package_specifier(manifest.main())
            .map_err(|error| PackageError::ModuleSpecifier(error.to_string()))?;
        Ok(Self {
            manifest,
            module_loader,
            entrypoint,
        })
    }
}

impl<B> Deref for PluginPackage<B> {
    type Target = PluginManifest<B>;

    fn deref(&self) -> &PluginManifest<B> {
        &self.manifest
    }
}

impl<B> crate::plugin::PluginInstance for PluginPackage<B>
where
    B: Send + Sync + 'static,
{
    fn id(&self) -> &str {
        self.manifest.id()
    }

    fn version(&self) -> &str {
        self.manifest.version()
    }

    fn class(&self) -> &str {
        self.manifest.class()
    }

    fn module_loader(&self) -> &crate::loading::PluginModuleLoader {
        &self.module_loader
    }

    fn entrypoint(&self) -> &deno_core::ModuleSpecifier {
        &self.entrypoint
    }

    fn caches(&self) -> &[PluginCache] {
        self.manifest.caches()
    }

    fn config(&self) -> &PluginConfig {
        self.manifest.config()
    }

    fn meta(&self) -> &polymer_pluginmanifest::PluginMeta {
        self.manifest.meta()
    }

    fn api(&self) -> &semver::VersionReq {
        self.manifest.api()
    }

    fn manifest(&self) -> &serde_json::Value {
        self.manifest.document()
    }
}
