use super::{GameCatalog, GameCatalogError, Launchable, PathBuf, Uuid};
use serde_json::Value;

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

    pub async fn launchable(&self, id: Uuid) -> Result<Launchable, GameCatalogError> {
        let connection = self.connection().await?;
        let mut rows = connection.query(
            "SELECT l.binary_id, l.path, b.blake3, b.serial, b.title_id, b.parent_binary_id, b.platform, m.manifest_json FROM launchables l JOIN binaries b ON b.id = l.binary_id LEFT JOIN launchable_manifests m ON m.launchable_id = l.id WHERE l.id = ?1",
            [Self::id(id)],
        ).await?;
        let row = rows
            .next()
            .await?
            .ok_or_else(|| GameCatalogError::NotFound(id.to_string()))?;
        Ok(Launchable {
            manifest: Self::decode_manifest(row.get_value(7)?)?,
            id,
            binary_id: Self::uuid(row.get_value(0)?)?,
            path: PathBuf::from(row.get::<String>(1)?),
            binary_blake3: Self::blob(row.get_value(2)?)?.into_boxed_slice(),
            serial: Self::optional_text(row.get_value(3)?)?,
            title_id: Self::optional_text(row.get_value(4)?)?,
            parent_binary_id: Self::optional_uuid(row.get_value(5)?)?,
            platform: row.get(6)?,
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
}
