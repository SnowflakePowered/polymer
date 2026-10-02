use turso::{Value, transaction::Transaction};
use uuid::Uuid;

use super::{GameCatalog, GameCatalogError};
use crate::games::{DiscSets, IdentifiedLaunchable, Serial};

const BINARY_NAMESPACE: Uuid = Uuid::from_u128(0x599c9d3b98265d0080a77ae753dec395);

impl GameCatalog {
    /// Discs of one game can share an executable, so the normalized serial and header
    /// disc number are part of a binary's identity.
    pub(super) fn binary_id(
        blake3: &[u8],
        platform: &str,
        serial: Option<&str>,
        disc_number: Option<u8>,
    ) -> Uuid {
        let serial = serial.map(|serial| Serial::normalize(platform, serial));
        if serial.is_none() && disc_number.is_none() {
            return Uuid::new_v5(&BINARY_NAMESPACE, blake3);
        }
        let mut name = blake3.to_vec();
        name.extend_from_slice(serial.as_deref().unwrap_or_default().as_bytes());
        name.push(0);
        name.extend(disc_number);
        Uuid::new_v5(&BINARY_NAMESPACE, &name)
    }

    /// The root binary holding the first disc of the set `input` belongs to, when `input`
    /// is a later disc imported without an explicit parent.
    pub(super) async fn disc_parent(
        &self,
        transaction: &Transaction<'_>,
        input: &IdentifiedLaunchable,
        binary_id: Uuid,
    ) -> Result<Option<Uuid>, GameCatalogError> {
        let (None, Some(serial), Some(disc @ 2..)) = (
            input.parent_binary_id,
            input.serial.as_deref(),
            input.disc(),
        ) else {
            return Ok(input.parent_binary_id);
        };
        let Some((set, firsts)) = DiscSets::get().first_discs(&input.platform, serial) else {
            return Ok(None);
        };
        let mut rows = transaction
            .query(
                "SELECT id, serial, disc_number FROM binaries WHERE platform = ?1 AND parent_binary_id IS NULL AND serial IS NOT NULL AND id != ?2",
                vec![Value::from(input.platform.clone()), Self::id(binary_id)],
            )
            .await?;
        while let Some(row) = rows.next().await? {
            let serial: String = row.get(1)?;
            let first = DiscSets::get()
                .disc(
                    &input.platform,
                    Some(&serial),
                    Self::optional_disc(row.get_value(2)?)?,
                )
                .is_none_or(|disc| disc == 1);
            if first && firsts.contains(&Serial::normalize(&input.platform, &serial).as_ref()) {
                return Ok(Some(Self::uuid(row.get_value(0)?)?));
            }
        }
        Err(GameCatalogError::MissingFirstDisc {
            set: set.to_owned(),
            disc,
        })
    }

    pub(super) fn optional_disc(value: Value) -> Result<Option<u8>, GameCatalogError> {
        match value {
            Value::Null => Ok(None),
            Value::Integer(disc) => u8::try_from(disc)
                .map(Some)
                .map_err(|_| GameCatalogError::InvalidData("invalid disc number".to_owned())),
            _ => Err(GameCatalogError::InvalidData(
                "expected database disc number".to_owned(),
            )),
        }
    }
}
