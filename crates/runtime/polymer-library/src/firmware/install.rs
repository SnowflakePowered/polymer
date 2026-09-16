use super::{FirmwareDescription, FirmwareError, FirmwareStore, bios::BiosFile};
use std::path::Path;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

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
        let transaction = connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;
        transaction.execute(
                "INSERT INTO firmware (platform, type, label, digest, byte_length) VALUES (?, ?, ?, ?, ?)
                 ON CONFLICT(platform, type, label) DO UPDATE SET digest = excluded.digest, byte_length = excluded.byte_length",
                turso::params![item.platform.clone(), item.firmware_type.clone(), item.label.clone(), bios.digest.clone(), item.byte_length as i64],
            ).await?;
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
        let temporary = directory.join(format!(".{}.tmp", Uuid::now_v7().simple()));
        let result = async {
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .await?;
            file.write_all(&bios.contents).await?;
            file.sync_all().await?;
            drop(file);
            // Publishing by hard link is atomic and never overwrites an existing object.
            match tokio::fs::hard_link(&temporary, &destination).await {
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
                result => result,
            }
        }
        .await;
        let _ = tokio::fs::remove_file(&temporary).await;
        result?;
        Ok(())
    }
}
