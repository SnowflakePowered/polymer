use super::{
    EmulatorPlugin, PackageError, PlatformPlugin, ScraperPlugin, ThemePlugin,
    descriptor::PluginDescriptor,
};
use crate::{
    emulator::EmulatorPluginInstance, platform::PlatformPluginInstance, plugin::PluginInstance,
    scraper::ScraperPluginInstance, theme::ThemePluginInstance,
};
use polymer_distribution::{Index, SignedArchive};
use polymer_pluginmanifest::{AnyPluginManifest, PackageArchive, PackageSource};
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
    jsr_overrides: Arc<dashmap::DashMap<String, PathBuf>>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolves a JSR package such as `@polymer-fe/emulator-updates` from a local directory
    /// holding its `jsr.json`, for every plugin.
    pub fn override_jsr(&self, package: impl Into<String>, directory: PathBuf) {
        self.jsr_overrides.insert(package.into(), directory);
    }

    pub(crate) fn jsr_overrides(&self) -> Arc<dashmap::DashMap<String, PathBuf>> {
        self.jsr_overrides.clone()
    }

    pub fn platform(&self, platform: &str) -> Option<Arc<dyn PlatformPluginInstance>> {
        self.plugins::<dyn PlatformPluginInstance>()
            .into_iter()
            .find(|plugin| plugin.platforms().iter().any(|id| id == platform))
    }

    /// Loads every plugin under `search_paths`, earlier paths first. Unpacked plugin
    /// directories load before signed archives, so an unpacked plugin takes the place of an
    /// archive with the same ID, and archives load newest version first, so the newest of
    /// several archives of a plugin is the one loaded. Returns the plugins that could not be
    /// loaded.
    pub fn discover(
        &self,
        search_paths: &[PathBuf],
        index: &Index,
    ) -> Vec<(PathBuf, PluginRegistryError)> {
        let mut unpacked = Vec::new();
        let mut archives = Vec::new();
        for root in search_paths {
            Self::search(root, &mut unpacked, &mut archives);
        }
        archives.sort_by_cached_key(|path| {
            let version = fs::read(path).ok().and_then(|file| {
                Some(
                    SignedArchive::split(&file)
                        .ok()?
                        .trusted_comment()
                        .ok()?
                        .version,
                )
            });
            std::cmp::Reverse(version)
        });
        let unpacked = unpacked.into_iter().map(|path| {
            let result = self.load(&path);
            (path, result)
        });
        let archives = archives.into_iter().map(|path| {
            let result = self.load_archive(&path, index);
            (path, result)
        });
        unpacked
            .chain(archives)
            .filter_map(|(path, result)| result.err().map(|error| (path, error)))
            .collect()
    }

    /// Collects the plugin directories and archives under `root`, in path order.
    fn search(root: &Path, unpacked: &mut Vec<PathBuf>, archives: &mut Vec<PathBuf>) {
        let mut pending = vec![root.to_owned()];
        while let Some(directory) = pending.pop() {
            if directory != root && directory.join("plugin.toml").is_file() {
                unpacked.push(directory);
                continue;
            }
            let Ok(entries) = fs::read_dir(&directory) else {
                continue;
            };
            let mut children = entries
                .filter_map(Result::ok)
                .filter(|entry| {
                    !matches!(entry.file_name().to_str(), Some("node_modules" | ".git"))
                })
                .filter_map(|entry| Some((entry.path(), entry.file_type().ok()?)))
                .collect::<Vec<_>>();
            children.sort_by(|(left, _), (right, _)| left.cmp(right));
            for (child, kind) in children.into_iter().rev() {
                if kind.is_dir() {
                    pending.push(child);
                } else if kind.is_file()
                    && child
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("asar"))
                {
                    archives.push(child);
                }
            }
        }
    }

    /// Loads an unpacked plugin from the directory holding its `plugin.toml`, or from the
    /// `plugin.toml` itself.
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
        self.load_source(PackageSource::directory(root)?)
    }

    /// Loads a signed plugin archive once one of the keys `index` registers for the plugin it
    /// names has verified its signature.
    pub fn load_archive(
        &self,
        path: impl AsRef<Path>,
        index: &Index,
    ) -> Result<Arc<dyn PluginInstance>, PluginRegistryError> {
        let path = path.as_ref();
        let bytes: Arc<[u8]> = fs::read(path)?.into();
        let signed = SignedArchive::split(&bytes)?;
        signed.verify(&index.keys(&signed.trusted_comment()?.id))?;
        let archive = PackageArchive::new(path.to_owned(), bytes.clone())?;
        self.load_source(PackageSource::Archive(archive))
    }

    /// Registers every interface implemented by a package. A plugin ID can be registered
    /// only once.
    fn load_source(
        &self,
        source: PackageSource,
    ) -> Result<Arc<dyn PluginInstance>, PluginRegistryError> {
        let manifest = AnyPluginManifest::load(source)?;
        let host = crate::plugin::HostApi::for_class(manifest.class());
        if !manifest.api().matches(host) {
            return Err(PluginRegistryError::IncompatibleApi {
                id: manifest.id().to_owned(),
                required: manifest.api().clone(),
                host: host.clone(),
            });
        }
        let mut registered;
        let instance: Arc<dyn PluginInstance> = match manifest {
            AnyPluginManifest::Platform(manifest) => {
                let concrete = Arc::new(PlatformPlugin::new(manifest)?);
                registered = RegisteredPlugin::new(concrete.clone());
                registered.insert::<dyn PlatformPluginInstance>(concrete.clone());
                concrete
            }
            AnyPluginManifest::Emulator(manifest) => {
                let concrete = Arc::new(EmulatorPlugin::new(manifest)?);
                registered = RegisteredPlugin::new(concrete.clone());
                registered.insert::<dyn EmulatorPluginInstance>(concrete.clone());
                concrete
            }
            AnyPluginManifest::Scraper(manifest) => {
                let concrete = Arc::new(ScraperPlugin::new(manifest)?);
                registered = RegisteredPlugin::new(concrete.clone());
                registered.insert::<dyn ScraperPluginInstance>(concrete.clone());
                concrete
            }
            AnyPluginManifest::Theme(manifest) => {
                let concrete = Arc::new(ThemePlugin::new(manifest)?);
                registered = RegisteredPlugin::new(concrete.clone());
                registered.insert::<dyn ThemePluginInstance>(concrete.clone());
                concrete
            }
        };
        self.register(instance.id(), registered)?;
        Ok(instance)
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
        if let Some(provider) = registered.get::<dyn PlatformPluginInstance>() {
            for existing in plugins
                .values()
                .filter_map(|item| item.get::<dyn PlatformPluginInstance>())
            {
                for platform in provider.platforms() {
                    if existing.platforms().contains(platform) {
                        return Err(PluginRegistryError::DuplicatePlatform {
                            platform: platform.clone(),
                            existing: existing.id().to_owned(),
                            incoming: id.to_owned(),
                        });
                    }
                }
            }
        }
        plugins.insert(id.to_owned(), registered);
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum PluginRegistryError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Manifest(#[from] toml::de::Error),
    #[error(transparent)]
    Package(#[from] PackageError),
    #[error(transparent)]
    Distribution(#[from] polymer_distribution::Error),
    #[error("plugin manifest has no package directory: {}", .0.display())]
    InvalidManifestPath(PathBuf),
    #[error("plugin '{0}' is already registered")]
    DuplicatePlugin(String),
    #[error("plugin '{id}' requires host API {required}, but this host provides {host}")]
    IncompatibleApi {
        id: String,
        required: semver::VersionReq,
        host: semver::Version,
    },
    #[error("platform '{platform}' has duplicate providers '{existing}' and '{incoming}'")]
    DuplicatePlatform {
        platform: String,
        existing: String,
        incoming: String,
    },
}
