use super::{GameCatalog, GameCatalogError, IdentifiedLaunchable, Launchable, LaunchableUri, Uuid};
use crate::PlatformSettings;
use serde_json::Value;
use std::collections::BTreeMap;

impl GameCatalog {
    pub async fn launchables_for_binary(
        &self,
        binary_id: Uuid,
    ) -> Result<Vec<Launchable>, GameCatalogError> {
        let connection = self.connection().await?;
        let mut rows = connection
            .query(
                "SELECT id FROM launchables WHERE binary_id = ?1 ORDER BY path",
                [Self::id(binary_id)],
            )
            .await?;
        let mut ids = Vec::new();
        while let Some(row) = rows.next().await? {
            ids.push(Self::uuid(row.get_value(0)?)?);
        }
        drop(rows);
        let mut launchables = Vec::new();
        for id in ids {
            launchables.push(self.launchable(id).await?);
        }
        Ok(launchables)
    }

    pub async fn launchable_at(
        &self,
        path: &LaunchableUri,
    ) -> Result<Option<Launchable>, GameCatalogError> {
        let connection = self.connection().await?;
        let mut rows = connection
            .query(
                "SELECT id FROM launchables WHERE path = ?1",
                [turso::Value::from(path.url()?.to_string())],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let id = Self::uuid(row.get_value(0)?)?;
        drop(rows);
        self.launchable(id).await.map(Some)
    }

    pub async fn launchable(&self, id: Uuid) -> Result<Launchable, GameCatalogError> {
        let connection = self.connection().await?;
        let mut rows = connection.query(
            "SELECT l.binary_id, l.path, b.blake3, b.serial, b.title_id, b.parent_binary_id, b.platform, m.manifest_json, l.entrypoint, l.package, b.disc_number, b.requires_license FROM launchables l JOIN binaries b ON b.id = l.binary_id LEFT JOIN launchable_manifests m ON m.launchable_id = l.id WHERE l.id = ?1",
            [Self::id(id)],
        ).await?;
        let row = rows
            .next()
            .await?
            .ok_or_else(|| GameCatalogError::NotFound(id.to_string()))?;
        Ok(Launchable {
            id,
            binary_id: Self::uuid(row.get_value(0)?)?,
            identified: IdentifiedLaunchable {
                manifest: Self::decode_manifest(row.get_value(7)?)?,
                path: row.get::<String>(1)?.parse()?,
                entrypoint: Self::optional_text(row.get_value(8)?)?,
                package: Self::optional_text(row.get_value(9)?)?,
                binary_blake3: Self::blob(row.get_value(2)?)?.try_into().map_err(|_| {
                    GameCatalogError::InvalidData("invalid binary digest".to_owned())
                })?,
                serial: Self::optional_text(row.get_value(3)?)?,
                title_id: Self::optional_text(row.get_value(4)?)?,
                disc_number: Self::optional_disc(row.get_value(10)?)?,
                requires_license: row.get::<i64>(11)? != 0,
                parent_binary_id: Self::optional_uuid(row.get_value(5)?)?,
                platform: row.get(6)?,
            },
        })
    }

    pub fn validate_configuration(values: &Value) -> Result<(), GameCatalogError> {
        let valid = values.as_object().is_some_and(|groups| {
            groups.values().all(|group| {
                group.as_object().is_some_and(|options| {
                    options.values().all(|value| {
                        matches!(
                            value,
                            Value::Null | Value::Bool(_) | Value::String(_) | Value::Number(_)
                        )
                    })
                })
            })
        });
        if !valid {
            return Err(GameCatalogError::InvalidData(
                "configuration must contain groups of scalar values".into(),
            ));
        }
        Ok(())
    }

    pub async fn configuration(
        &self,
        id: Uuid,
        plugin: &str,
        schema: &str,
    ) -> Result<Option<Value>, GameCatalogError> {
        self.launchable(id).await?;
        let connection = self.connection().await?;
        let mut rows = connection.query(
            "SELECT values_json FROM launchable_configurations WHERE launchable_id = ?1 AND plugin_id = ?2 AND schema = ?3",
            turso::params![Self::id(id), plugin, schema],
        ).await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let values = serde_json::from_str(&row.get::<String>(0)?)
            .map_err(|error| GameCatalogError::InvalidData(error.to_string()))?;
        Self::validate_configuration(&values)?;
        Ok(Some(values))
    }

    pub async fn save_configuration(
        &self,
        id: Uuid,
        plugin: &str,
        schema: &str,
        values: &Value,
    ) -> Result<(), GameCatalogError> {
        Self::validate_configuration(values)?;
        if plugin.is_empty() || schema.is_empty() {
            return Err(GameCatalogError::InvalidData(
                "configuration plugin and schema must be nonempty".into(),
            ));
        }
        self.connection().await?.execute(
            "INSERT INTO launchable_configurations (launchable_id, plugin_id, schema, values_json) VALUES (?1, ?2, ?3, ?4) ON CONFLICT (launchable_id, plugin_id, schema) DO UPDATE SET values_json = excluded.values_json",
            turso::params![Self::id(id), plugin, schema, values.to_string()],
        ).await?;
        Ok(())
    }

    pub async fn launchable_emulator(&self, id: Uuid) -> Result<Option<String>, GameCatalogError> {
        let connection = self.connection().await?;
        let mut rows = connection
            .query(
                "SELECT plugin_id FROM launchable_emulators WHERE launchable_id = ?1",
                [Self::id(id)],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        Ok(Some(row.get::<String>(0)?))
    }

    pub async fn set_launchable_emulator(
        &self,
        id: Uuid,
        emulator: Option<&str>,
    ) -> Result<(), GameCatalogError> {
        self.launchable(id).await?;
        let connection = self.connection().await?;
        match emulator.filter(|emulator| !emulator.is_empty()) {
            Some(emulator) => connection.execute(
                "INSERT INTO launchable_emulators (launchable_id, plugin_id) VALUES (?1, ?2) ON CONFLICT (launchable_id) DO UPDATE SET plugin_id = excluded.plugin_id",
                turso::params![Self::id(id), emulator],
            ).await?,
            None => connection.execute(
                "DELETE FROM launchable_emulators WHERE launchable_id = ?1",
                [Self::id(id)],
            ).await?,
        };
        Ok(())
    }

    pub async fn platform_settings(
        &self,
    ) -> Result<BTreeMap<String, PlatformSettings>, GameCatalogError> {
        let connection = self.connection().await?;
        let mut rows = connection
            .query("SELECT platform, emulator FROM platform_settings", ())
            .await?;
        let mut settings = BTreeMap::new();
        while let Some(row) = rows.next().await? {
            settings.insert(
                row.get::<String>(0)?,
                PlatformSettings {
                    emulator: Self::optional_text(row.get_value(1)?)?,
                },
            );
        }
        Ok(settings)
    }

    pub async fn save_platform_settings(
        &self,
        platform: &str,
        settings: &PlatformSettings,
    ) -> Result<(), GameCatalogError> {
        if platform.is_empty() || settings.emulator.as_deref() == Some("") {
            return Err(GameCatalogError::InvalidData(
                "platform and emulator must be nonempty".into(),
            ));
        }
        let emulator = settings
            .emulator
            .clone()
            .map_or(turso::Value::Null, turso::Value::Text);
        self.connection().await?.execute(
            "INSERT INTO platform_settings (platform, emulator) VALUES (?1, ?2) ON CONFLICT (platform) DO UPDATE SET emulator = excluded.emulator",
            turso::params![platform, emulator],
        ).await?;
        Ok(())
    }
}
