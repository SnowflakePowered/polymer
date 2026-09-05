use dashmap::DashMap;
use polymer_library::GameCatalog;
use polymer_pluginhost::runtime::PluginRuntimeHandle;
use polymer_scraping::Package;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

pub(crate) type ConfigurationLocks = Arc<DashMap<(Uuid, String), Arc<Mutex<()>>>>;

pub(crate) struct LaunchableConfigurations {
    catalog: Arc<GameCatalog>,
    plugins: PluginRuntimeHandle,
    locks: ConfigurationLocks,
}

impl LaunchableConfigurations {
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
        let package = tokio::task::spawn_blocking(move || {
            let mut package = Package::identify(
                &launchable.path,
                Some(&launchable.platform),
                launchable.parent_binary_id,
            )
            .map_err(|error| error.to_string())?;
            let digest = blake3::Hash::from_bytes(
                launchable
                    .binary_blake3
                    .as_ref()
                    .try_into()
                    .map_err(|_| "invalid library binary digest")?,
            )
            .to_hex();
            if package.binary_digest != digest.as_str() {
                return Err(
                    "launchable binary changed; update its library registration before configuring"
                        .into(),
                );
            }
            package.serial = launchable.serial;
            package.title_id = launchable.title_id;
            Ok::<_, String>(package)
        })
        .await
        .map_err(|error| error.to_string())??;
        Ok(package)
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
            preferred,
        )
        .await?;
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
