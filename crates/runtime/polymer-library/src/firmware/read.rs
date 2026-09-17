use super::{FirmwareDescription, FirmwareError, FirmwareLookup, FirmwareStore};
use std::path::PathBuf;

impl FirmwareStore {
    pub async fn list(&self, platform: &str) -> Result<Vec<FirmwareDescription>, FirmwareError> {
        let mut connection = self.database.connect()?;
        let transaction = connection.transaction().await?;
        let mut rows = transaction
            .query(
                "SELECT label, byte_length FROM firmware WHERE platform = ? ORDER BY label",
                [platform],
            )
            .await?;
        let mut result = Vec::new();
        while let Some(row) = rows.next().await? {
            let label: String = row.get(0)?;
            let mut aliases = transaction.query(
                "SELECT name FROM firmware_names WHERE platform = ? AND label = ? ORDER BY position",
                turso::params![platform, label.clone()],
            ).await?;
            let mut names = Vec::new();
            while let Some(alias) = aliases.next().await? {
                names.push(alias.get(0)?);
            }
            result.push(FirmwareDescription {
                platform: platform.into(),
                label,
                names,
                byte_length: row.get::<i64>(1)? as u64,
            });
        }
        drop(rows);
        transaction.commit().await?;
        Ok(result)
    }

    /// Resolve once so opened resources retain their immutable content across replacement.
    pub async fn file(
        &self,
        platform: &str,
        lookup: &FirmwareLookup,
    ) -> Result<(String, PathBuf), FirmwareError> {
        let (algorithm, value) = match lookup {
            FirmwareLookup::Name(value) => ("name", value.clone()),
            FirmwareLookup::Md5(value) => ("md5", value.to_ascii_lowercase()),
            FirmwareLookup::Sha1(value) => ("sha1", value.to_ascii_lowercase()),
            FirmwareLookup::Sha256(value) => ("sha256", value.to_ascii_lowercase()),
        };
        let connection = self.database.connect()?;
        let mut rows = connection
            .query(
                "SELECT label, digest FROM firmware WHERE platform = ? AND label IN (
                SELECT label FROM firmware_names WHERE platform = ? AND ? = 'name' AND name = ?
                UNION
                SELECT label FROM firmware_hashes WHERE platform = ? AND algorithm = ? AND hash = ?
            ) LIMIT 2",
                turso::params![
                    platform,
                    platform,
                    algorithm,
                    value.clone(),
                    platform,
                    algorithm,
                    value.clone()
                ],
            )
            .await?;
        let row = rows
            .next()
            .await?
            .ok_or_else(|| FirmwareError::NotFound(format!("{platform}/{algorithm}/{value}")))?;
        if rows.next().await?.is_some() {
            return Err(FirmwareError::Invalid(
                "ambiguous firmware lookup; use a unique hash".into(),
            ));
        }
        let digest: String = row.get(1)?;
        Ok((row.get(0)?, self.object_path(platform, &digest)?))
    }

    fn object_path(&self, platform: &str, digest: &str) -> Result<PathBuf, FirmwareError> {
        if !Self::valid_platform(platform)
            || digest.len() != 64
            || !digest.bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err(FirmwareError::Invalid("invalid stored object path".into()));
        }
        Ok(self.root.join(platform).join("objects").join(digest))
    }
}
