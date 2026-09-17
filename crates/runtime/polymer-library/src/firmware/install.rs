use super::{FirmwareDescription, FirmwareError, FirmwareStore, bios::BiosFile};
use std::path::Path;

impl FirmwareStore {
    /// Recognize the complete file using consolespec BIOS hashes and install the first matching entry.
    pub async fn install_bios(
        &self,
        path: &Path,
    ) -> Result<Option<FirmwareDescription>, FirmwareError> {
        let path = path.to_owned();
        let Some(bios) = tokio::task::spawn_blocking(move || BiosFile::read(&path))
            .await
            .map_err(|error| {
                FirmwareError::Invalid(format!("BIOS detection task failed: {error}"))
            })??
        else {
            return Ok(None);
        };
        let item = &bios.description;
        self.stage(&item.platform, &bios).await?;
        let mut connection = self.database.connect()?;
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .await?;
        let transaction = connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;
        transaction.execute(
                "INSERT INTO firmware (platform, label, digest, byte_length) VALUES (?, ?, ?, ?)
                 ON CONFLICT(platform, label) DO UPDATE SET digest = excluded.digest, byte_length = excluded.byte_length",
                turso::params![item.platform.clone(), item.label.clone(), bios.digest.clone(), item.byte_length as i64],
            ).await?;
        transaction
            .execute(
                "DELETE FROM firmware_names WHERE platform = ? AND label = ?",
                turso::params![item.platform.clone(), item.label.clone()],
            )
            .await?;
        for (position, name) in item.names.iter().enumerate() {
            transaction.execute("INSERT OR IGNORE INTO firmware_names(platform, label, name, position) VALUES (?, ?, ?, ?)", turso::params![item.platform.clone(), item.label.clone(), name.clone(), position as i64]).await?;
        }
        transaction
            .execute(
                "DELETE FROM firmware_hashes WHERE platform = ? AND label = ?",
                turso::params![item.platform.clone(), item.label.clone()],
            )
            .await?;
        for (algorithm, hash) in [
            ("md5", bios.definition.md5()),
            ("sha1", bios.definition.sha1()),
            ("sha256", bios.definition.sha256()),
        ] {
            let Some(hash) = hash else { continue };
            transaction.execute("INSERT INTO firmware_hashes(platform, label, algorithm, hash) VALUES (?, ?, ?, ?) ON CONFLICT(platform, label, algorithm) DO UPDATE SET hash = excluded.hash", turso::params![item.platform.clone(), item.label.clone(), algorithm, hash.to_ascii_lowercase()]).await?;
        }
        transaction.commit().await?;
        Ok(Some(bios.description))
    }

    async fn stage(&self, platform: &str, bios: &BiosFile) -> Result<(), FirmwareError> {
        let digest = &bios.digest;
        let directory = self.root.join(platform).join("objects");
        tokio::fs::create_dir_all(&directory).await?;
        let destination = directory.join(digest);
        if tokio::fs::try_exists(&destination).await? {
            return Ok(());
        }
        let mut source = bios.file.try_clone()?;
        let byte_length = bios.description.byte_length;
        tokio::task::spawn_blocking(move || -> Result<(), FirmwareError> {
            use std::io::{Read, Seek};
            source.rewind()?;
            let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
            let copied = std::io::copy(&mut source.take(byte_length), &mut temporary)?;
            if copied != byte_length {
                return Err(FirmwareError::Invalid(
                    "BIOS input changed during installation".into(),
                ));
            }
            temporary.as_file().sync_all()?;
            match std::fs::hard_link(temporary.path(), destination) {
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
                result => result.map_err(FirmwareError::from),
            }
        })
        .await
        .map_err(|error| FirmwareError::Invalid(format!("BIOS copy task failed: {error}")))?
    }
}
