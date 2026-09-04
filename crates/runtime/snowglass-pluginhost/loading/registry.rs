use super::{
    EmulatorPlugin, PackageError, ScraperPlugin, ThemePlugin, descriptor::PluginDescriptor,
};
use crate::{
    emulator::EmulatorPluginInstance, plugin::PluginInstance, scraper::ScraperPluginInstance,
    theme::ThemePluginInstance,
};
use serde::Deserialize;
use std::{
    any::{Any, TypeId},
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};
use thiserror::Error;

struct RegisteredPlugin {
    interfaces: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
}

impl RegisteredPlugin {
    fn new(instance: Arc<dyn PluginInstance>) -> Self {
        let mut registered = Self {
            interfaces: HashMap::new(),
        };
        registered.insert(instance);
        registered
    }

    fn insert<I>(&mut self, instance: Arc<I>)
    where
        I: PluginInstance + ?Sized,
        Arc<I>: Any + Send + Sync,
    {
        self.interfaces
            .insert(TypeId::of::<I>(), Box::new(instance));
    }

    fn get<I>(&self) -> Option<Arc<I>>
    where
        I: PluginInstance + ?Sized,
        Arc<I>: Any + Send + Sync,
    {
        self.interfaces
            .get(&TypeId::of::<I>())?
            .downcast_ref::<Arc<I>>()
            .cloned()
    }
}

#[derive(Clone, Default)]
pub struct PluginRegistry {
    plugins: Arc<RwLock<HashMap<String, RegisteredPlugin>>>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads the package containing `plugin.toml` and registers every interface
    /// implemented by that package. A plugin ID can be registered only once.
    pub fn load(
        &self,
        manifest: impl AsRef<Path>,
    ) -> Result<Arc<dyn PluginInstance>, PluginRegistryError> {
        let manifest = manifest.as_ref();
        let manifest = if manifest.is_dir() {
            manifest.join("plugin.toml")
        } else {
            manifest.to_owned()
        }
        .canonicalize()?;
        let root = manifest
            .parent()
            .ok_or_else(|| PluginRegistryError::InvalidManifestPath(manifest.clone()))?;
        let header: ManifestHeader = toml::from_str(&fs::read_to_string(&manifest)?)?;

        match header.plugin.class.as_str() {
            "emulator" => {
                let concrete = Arc::new(EmulatorPlugin::load(root)?);
                let instance: Arc<dyn PluginInstance> = concrete.clone();
                let emulator: Arc<dyn EmulatorPluginInstance> = concrete;
                let mut registered = RegisteredPlugin::new(instance.clone());
                registered.insert(emulator);
                self.register(instance.id(), registered)?;
                Ok(instance)
            }
            "scraper" => {
                let concrete = Arc::new(ScraperPlugin::load(root)?);
                let instance: Arc<dyn PluginInstance> = concrete.clone();
                let scraper: Arc<dyn ScraperPluginInstance> = concrete;
                let mut registered = RegisteredPlugin::new(instance.clone());
                registered.insert(scraper);
                self.register(instance.id(), registered)?;
                Ok(instance)
            }
            "theme" => {
                let concrete = Arc::new(ThemePlugin::load(root)?);
                let instance: Arc<dyn PluginInstance> = concrete.clone();
                let theme: Arc<dyn ThemePluginInstance> = concrete;
                let mut registered = RegisteredPlugin::new(instance.clone());
                registered.insert(theme);
                self.register(instance.id(), registered)?;
                Ok(instance)
            }
            class => Err(PluginRegistryError::UnknownClass(class.to_owned())),
        }
    }

    /// Retrieves one registered interface for a plugin. This is a typed
    /// capability cast rather than a Rust trait-object upcast.
    pub fn plugin<I>(&self, id: &str) -> Option<Arc<I>>
    where
        I: PluginInstance + ?Sized,
        Arc<I>: Any + Send + Sync,
    {
        self.plugins.read().unwrap().get(id)?.get::<I>()
    }

    /// Retrieves every registered implementation of an interface, ordered by
    /// plugin ID so callers can present a stable selection to users.
    pub fn plugins<I>(&self) -> Vec<Arc<I>>
    where
        I: PluginInstance + ?Sized,
        Arc<I>: Any + Send + Sync,
    {
        let plugins = self.plugins.read().unwrap();
        let mut ids = plugins.keys().collect::<Vec<_>>();
        ids.sort();
        ids.into_iter()
            .filter_map(|id| plugins[id].get::<I>())
            .collect()
    }

    /// Returns a stable frontend-safe snapshot ordered by plugin ID.
    pub fn descriptors(&self) -> Vec<PluginDescriptor> {
        self.plugins::<dyn PluginInstance>()
            .iter()
            .map(|plugin| PluginDescriptor::from_instance(plugin.as_ref()))
            .collect()
    }

    fn register(&self, id: &str, registered: RegisteredPlugin) -> Result<(), PluginRegistryError> {
        let mut plugins = self.plugins.write().unwrap();
        if plugins.contains_key(id) {
            return Err(PluginRegistryError::DuplicatePlugin(id.to_owned()));
        }
        plugins.insert(id.to_owned(), registered);
        Ok(())
    }
}

#[derive(Deserialize)]
struct ManifestHeader {
    plugin: ManifestPluginHeader,
}

#[derive(Deserialize)]
struct ManifestPluginHeader {
    class: String,
}

#[derive(Debug, Error)]
pub enum PluginRegistryError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Manifest(#[from] toml::de::Error),
    #[error(transparent)]
    Package(#[from] PackageError),
    #[error("plugin manifest has no package directory: {}", .0.display())]
    InvalidManifestPath(PathBuf),
    #[error("plugin class '{0}' is not registered")]
    UnknownClass(String),
    #[error("plugin '{0}' is already registered")]
    DuplicatePlugin(String),
}
