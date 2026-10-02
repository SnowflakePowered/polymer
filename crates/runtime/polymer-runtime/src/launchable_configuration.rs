use crate::library::api::LibraryApi;
use dashmap::DashMap;
use polymer_library::{GameCatalog, PlatformSettings};
use polymer_pluginhost::{emulator::EmulatorPluginInstance, runtime::PluginRuntimeHandle};
use polymer_scraping::Package;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::Mutex;
use uuid::Uuid;

pub(crate) type ConfigurationLocks = Arc<DashMap<(Uuid, String), Arc<Mutex<()>>>>;

pub(crate) struct LaunchableConfigurations {
    catalog: Arc<GameCatalog>,
    plugins: PluginRuntimeHandle,
    locks: ConfigurationLocks,
}

impl LibraryApi for LaunchableConfigurations {}

impl LaunchableConfigurations {
    pub(crate) async fn launch_set(&self, ids: &[String]) -> Result<Vec<(Value, Package)>, String> {
        if ids.is_empty() {
            return Err("launchSet must not be empty".into());
        }
        let mut seen = std::collections::HashSet::new();
        let mut discs: Vec<(Value, Package)> = Vec::with_capacity(ids.len());
        for id in ids {
            let uuid = Uuid::parse_str(id).map_err(|error| error.to_string())?;
            if !seen.insert(uuid) {
                return Err("launchSet contains duplicate launchables".into());
            }
            let launchable = self
                .catalog
                .launchable(uuid)
                .await
                .map_err(|error| error.to_string())?;
            let value = Self::launchable_value(&launchable);
            let mut package = Package::from_launchable(launchable);
            package
                .prepare_launch()
                .map_err(|error| error.to_string())?;
            if discs
                .first()
                .is_some_and(|(_, first)| first.platform != package.platform)
            {
                return Err("all launchables must use the same console".into());
            }
            discs.push((value, package));
        }
        Ok(discs)
    }

    pub(crate) fn new(
        catalog: Arc<GameCatalog>,
        plugins: PluginRuntimeHandle,
        locks: ConfigurationLocks,
    ) -> Self {
        Self {
            catalog,
            plugins,
            locks,
        }
    }

    pub(crate) async fn package(&self, id: &str) -> Result<Package, String> {
        let id = Uuid::parse_str(id).map_err(|error| error.to_string())?;
        let launchable = self
            .catalog
            .launchable(id)
            .await
            .map_err(|error| error.to_string())?;
        Ok(Package::from_launchable(launchable))
    }

    pub(crate) async fn describe(
        &self,
        id: &str,
        defaults: crate::ConfigurationDefaults,
        preferred: Option<String>,
    ) -> Result<Value, String> {
        let package = self.package(id).await?;
        let uuid = Uuid::parse_str(id).map_err(|error| error.to_string())?;
        let mut description = crate::configuration::describe_api(
            self.plugins.clone(),
            defaults,
            package.clone(),
            preferred.clone(),
        )
        .await?;
        if preferred.is_none() {
            self.resolve_emulator(uuid, &package.platform, &mut description)
                .await?;
        }
        for candidate in description["candidates"]
            .as_array_mut()
            .ok_or("invalid emulator candidates")?
        {
            let emulator = candidate["id"]
                .as_str()
                .ok_or("missing emulator ID")?
                .to_owned();
            let initial = candidate["configuration"].take();
            let lock = self
                .locks
                .entry((uuid, emulator.clone()))
                .or_default()
                .clone();
            let _guard = lock.lock().await;
            candidate["configuration"] = self
                .settle(uuid, &emulator, &package, initial, None)
                .await?;
        }
        Ok(description)
    }

    pub(crate) async fn configure(
        &self,
        id: &str,
        emulator: &str,
        supplied: Option<Value>,
    ) -> Result<Value, String> {
        let uuid = Uuid::parse_str(id).map_err(|error| error.to_string())?;
        let lock = self
            .locks
            .entry((uuid, emulator.to_owned()))
            .or_default()
            .clone();
        let _guard = lock.lock().await;
        let package = self.package(id).await?;
        let initial = crate::configuration::configure_api(
            self.plugins.clone(),
            package.clone(),
            emulator.to_owned(),
            None,
        )
        .await?;
        self.settle(uuid, emulator, &package, initial, supplied)
            .await
    }

    async fn resolve_emulator(
        &self,
        id: Uuid,
        platform: &str,
        description: &mut Value,
    ) -> Result<(), String> {
        let game = self
            .catalog
            .launchable_emulator(id)
            .await
            .map_err(|error| error.to_string())?;
        let fallback = self
            .catalog
            .platform_settings()
            .await
            .map_err(|error| error.to_string())?
            .remove(platform)
            .and_then(|settings| settings.emulator);
        let selected = [&game, &fallback].into_iter().flatten().find(|emulator| {
            description["candidates"]
                .as_array()
                .is_some_and(|candidates| {
                    candidates
                        .iter()
                        .any(|candidate| candidate["id"] == **emulator)
                })
        });
        if let Some(selected) = selected {
            description["selectedEmulator"] = json!(selected);
        }
        description["gameEmulator"] = json!(game);
        description["platformEmulator"] = json!(fallback);
        Ok(())
    }

    fn emulators(&self, platform: &str) -> Vec<String> {
        self.plugins
            .registry()
            .plugins::<dyn EmulatorPluginInstance>()
            .iter()
            .filter(|plugin| plugin.platforms().iter().any(|value| value == platform))
            .map(|plugin| plugin.id().to_owned())
            .collect()
    }

    fn validate_emulator(&self, platform: &str, emulator: Option<&str>) -> Result<(), String> {
        match emulator {
            Some(emulator) if !self.emulators(platform).iter().any(|id| id == emulator) => Err(
                format!("Emulator plugin '{emulator}' does not support platform {platform}."),
            ),
            _ => Ok(()),
        }
    }

    pub(crate) async fn select(&self, id: &str, emulator: Option<String>) -> Result<(), String> {
        let package = self.package(id).await?;
        self.validate_emulator(&package.platform, emulator.as_deref())?;
        let uuid = Uuid::parse_str(id).map_err(|error| error.to_string())?;
        self.catalog
            .set_launchable_emulator(uuid, emulator.as_deref())
            .await
            .map_err(|error| error.to_string())
    }

    pub(crate) async fn platforms(&self) -> Result<Value, String> {
        let mut settings = self
            .catalog
            .platform_settings()
            .await
            .map_err(|error| error.to_string())?;
        let mut platforms = BTreeMap::<String, Vec<String>>::new();
        for plugin in self
            .plugins
            .registry()
            .plugins::<dyn EmulatorPluginInstance>()
        {
            for platform in plugin.platforms() {
                platforms
                    .entry(platform.clone())
                    .or_default()
                    .push(plugin.id().to_owned());
            }
        }
        Ok(platforms
            .into_iter()
            .map(|(platform, emulators)| {
                let settings = settings.remove(&platform).unwrap_or_default();
                json!({ "platform": platform, "emulators": emulators, "settings": settings })
            })
            .collect())
    }

    pub(crate) async fn update_platform(
        &self,
        platform: &str,
        settings: PlatformSettings,
    ) -> Result<Value, String> {
        let emulators = self.emulators(platform);
        if emulators.is_empty() {
            return Err(format!("No loaded emulator plugin supports {platform}."));
        }
        self.validate_emulator(platform, settings.emulator.as_deref())?;
        self.catalog
            .save_platform_settings(platform, &settings)
            .await
            .map_err(|error| error.to_string())?;
        Ok(json!({ "platform": platform, "emulators": emulators, "settings": settings }))
    }

    async fn settle(
        &self,
        id: Uuid,
        emulator: &str,
        package: &Package,
        mut configured: Value,
        supplied: Option<Value>,
    ) -> Result<Value, String> {
        if configured.is_null() {
            return Ok(Value::Null);
        }
        let schema = configured["schema"]
            .as_str()
            .filter(|value| !value.is_empty())
            .ok_or("configuration must declare a nonempty schema")?
            .to_owned();
        let stored = self
            .catalog
            .configuration(id, emulator, &schema)
            .await
            .map_err(|error| error.to_string())?;
        let reconfigure = stored.is_some() || supplied.is_some();
        let mut retained = stored.unwrap_or_else(|| json!({}));
        if let Some(values) = supplied {
            Self::merge(&mut retained, &values)?;
        }
        if reconfigure {
            configured = crate::configuration::configure_api(
                self.plugins.clone(),
                package.clone(),
                emulator.to_owned(),
                Some(retained.clone()),
            )
            .await?;
        }
        if configured["schema"].as_str() != Some(schema.as_str()) {
            return Err("plugin configuration schema changed during configuration".into());
        }
        Self::merge(&mut retained, &configured["values"])?;
        self.catalog
            .save_configuration(id, emulator, &schema, &retained)
            .await
            .map_err(|error| error.to_string())?;
        configured["values"] = retained;
        Ok(configured)
    }

    fn merge(retained: &mut Value, values: &Value) -> Result<(), String> {
        GameCatalog::validate_configuration(values).map_err(|error| error.to_string())?;
        for (group, options) in values.as_object().unwrap() {
            let target = retained
                .as_object_mut()
                .unwrap()
                .entry(group.clone())
                .or_insert_with(|| json!({}));
            target
                .as_object_mut()
                .unwrap()
                .extend(options.as_object().unwrap().clone());
        }
        Ok(())
    }
}
