use crate::{StorageKind, StorageManager};
use polymer_consolestorage::Error;
use serde_json::Value;
use uuid::Uuid;

/// Volume roles are determined by metadata, never by selection order.
#[derive(Default)]
pub struct StorageSelection {
    pub nand: Option<Uuid>,
    pub saves: Option<Uuid>,
    pub memory_card: Option<Uuid>,
    pub battery: Option<Uuid>,
    pub missing: Vec<Uuid>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn resolves_unordered_roles_and_preserves_missing_companions() {
        let root = tempfile::tempdir().unwrap();
        let manager = StorageManager::new(root.path());
        let nand = manager
            .create(StorageKind::Nand, "NINTENDO_WII", None)
            .await
            .unwrap()
            .metadata()
            .await
            .unwrap()
            .id;
        let saves = manager
            .create(StorageKind::SaveRepo, "NINTENDO_WII", None)
            .await
            .unwrap()
            .metadata()
            .await
            .unwrap()
            .id;
        for value in [json!([nand, saves]), json!([saves, nand])] {
            let selection = StorageSelection::resolve(&manager, "NINTENDO_WII", &value)
                .await
                .unwrap();
            assert_eq!(selection.nand, Some(nand));
            assert_eq!(selection.saves, Some(saves));
            assert!(selection.missing.is_empty());
        }
        let missing = Uuid::new_v4();
        let selection =
            StorageSelection::resolve(&manager, "NINTENDO_WII", &json!([missing, nand]))
                .await
                .unwrap();
        assert_eq!(selection.nand, Some(nand));
        assert_eq!(selection.saves, None);
        assert_eq!(selection.missing, vec![missing]);
        assert!(
            StorageSelection::resolve(&manager, "NINTENDO_WII", &json!([nand, nand]))
                .await
                .is_err()
        );
    }
}

impl StorageSelection {
    pub async fn resolve(
        manager: &StorageManager,
        platform: &str,
        value: &Value,
    ) -> Result<Self, Error> {
        let values = value
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_else(|| std::slice::from_ref(value));
        if values.is_empty() || values.len() > 2 {
            return Err(Error::Unsupported(
                "Select one volume or a NAND and save profile".into(),
            ));
        }
        let mut selection = Self::default();
        let mut seen = std::collections::BTreeSet::new();
        for value in values {
            let id = value
                .as_str()
                .and_then(|id| Uuid::parse_str(id).ok())
                .ok_or_else(|| {
                    Error::Unsupported("Storage selections require UUID strings".into())
                })?;
            if !seen.insert(id) {
                return Err(Error::Unsupported(
                    "A storage volume was selected more than once".into(),
                ));
            }
            let Some((kind, info)) = manager.volume_info(platform, id).await? else {
                selection.missing.push(id);
                continue;
            };
            if info.platform != platform {
                return Err(Error::Unsupported("Storage platform mismatch".into()));
            }
            selection.insert(kind, id)?;
        }
        selection.validate()?;
        Ok(selection)
    }

    fn insert(&mut self, kind: StorageKind, id: Uuid) -> Result<(), Error> {
        let slot = match kind {
            StorageKind::Nand => &mut self.nand,
            StorageKind::SaveRepo => &mut self.saves,
            StorageKind::MemoryCard => &mut self.memory_card,
            StorageKind::Battery => &mut self.battery,
        };
        if slot.replace(id).is_some() {
            return Err(Error::Unsupported(format!(
                "Multiple {} volumes selected for one device",
                kind.as_str()
            )));
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), Error> {
        let standalone = self.memory_card.is_some() || self.battery.is_some();
        let count = usize::from(self.nand.is_some())
            + usize::from(self.saves.is_some())
            + usize::from(self.memory_card.is_some())
            + usize::from(self.battery.is_some())
            + self.missing.len();
        if standalone && count > 1 {
            return Err(Error::Unsupported(
                "Memory cards and battery profiles cannot be combined with another volume".into(),
            ));
        }
        Ok(())
    }
}
