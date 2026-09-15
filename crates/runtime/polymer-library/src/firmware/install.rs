use super::{FirmwareDescription, FirmwareError, FirmwareInstallDescriptor, FirmwareStore};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

impl FirmwareStore {
    pub async fn install(
        &self,
        batch: &[FirmwareInstallDescriptor],
        platforms: &[String],
    ) -> Result<Vec<FirmwareDescription>, FirmwareError> {
        FirmwareInstallDescriptor::validate_batch(batch, platforms)?;
        let mut prepared = Vec::new();
        for item in batch {
            let digest = self.stage(item).await?;
            prepared.push((
                FirmwareDescription {
                    platform: item.platform.clone(),
                    firmware_type: item.firmware_type.clone(),
                    label: item.label.clone(),
                    byte_length: item.contents.len() as u64,
                },
                digest,
            ));
        }
        let mut connection = self.database.connect()?;
        let transaction = connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;
        for (item, digest) in &prepared {
            transaction.execute(
                "INSERT INTO firmware (platform, type, label, digest, byte_length) VALUES (?, ?, ?, ?, ?)
                 ON CONFLICT(platform, type, label) DO UPDATE SET digest = excluded.digest, byte_length = excluded.byte_length",
                turso::params![item.platform.clone(), item.firmware_type.clone(), item.label.clone(), digest.clone(), item.byte_length as i64],
            ).await?;
        }
        transaction.commit().await?;
        Ok(prepared.into_iter().map(|(item, _)| item).collect())
    }

    async fn stage(&self, item: &FirmwareInstallDescriptor) -> Result<String, FirmwareError> {
        let digest = blake3::hash(&item.contents).to_hex().to_string();
        let directory = self.root.join(&item.platform).join("objects");
        tokio::fs::create_dir_all(&directory).await?;
        let destination = directory.join(&digest);
        if tokio::fs::try_exists(&destination).await? {
            return Ok(digest);
        }
        let temporary = directory.join(format!(".{}.tmp", Uuid::now_v7().simple()));
        let result = async {
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .await?;
            file.write_all(&item.contents).await?;
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
        Ok(digest)
    }
}
