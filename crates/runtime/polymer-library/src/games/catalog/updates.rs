use super::{GameCatalog, GameCatalogError, Uuid};
use serde_json::Value;
use std::collections::BTreeMap;

impl GameCatalog {
    /// Replaces a title's cached update candidates with the result of a check, in its order.
    pub async fn replace_updates(
        &self,
        launchable: Uuid,
        candidates: &[Value],
        checked_at: i64,
    ) -> Result<(), GameCatalogError> {
        let mut connection = self.connection().await?;
        let transaction = connection.transaction().await?;
        transaction
            .execute(
                "DELETE FROM title_updates WHERE launchable_id = ?1",
                [Self::id(launchable)],
            )
            .await?;
        for (ordinal, candidate) in candidates.iter().enumerate() {
            let candidate = serde_json::to_string(candidate)
                .map_err(|error| GameCatalogError::InvalidData(error.to_string()))?;
            transaction
                .execute(
                    "INSERT INTO title_updates (launchable_id, ordinal, candidate_json, checked_at) VALUES (?1, ?2, ?3, ?4)",
                    (Self::id(launchable), ordinal as i64, candidate, checked_at),
                )
                .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    /// Every title's cached update candidates, in the order its last check returned them.
    pub(super) async fn updates(
        connection: &turso::Connection,
    ) -> Result<BTreeMap<Uuid, Vec<Value>>, GameCatalogError> {
        let mut rows = connection
            .query(
                "SELECT launchable_id, candidate_json FROM title_updates ORDER BY launchable_id, ordinal",
                (),
            )
            .await?;
        let mut updates = BTreeMap::<Uuid, Vec<Value>>::new();
        while let Some(row) = rows.next().await? {
            let candidate = serde_json::from_str(&row.get::<String>(1)?)
                .map_err(|error| GameCatalogError::InvalidData(error.to_string()))?;
            updates
                .entry(Self::uuid(row.get_value(0)?)?)
                .or_default()
                .push(candidate);
        }
        Ok(updates)
    }
}
