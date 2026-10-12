use super::{GameCatalog, GameCatalogError, IdentifiedLaunchable, Launchable, LaunchableUri, Uuid};

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
}
