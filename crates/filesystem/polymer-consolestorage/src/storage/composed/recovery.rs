use crate::{Error, StorageReader};

use super::ComposedStorage;
use super::journal::RenameRecord;
use super::layer::ComposedLayer;

impl ComposedStorage {
    pub fn recover(&self) -> Result<(), Error> {
        let _operation = self.operation.gate.write();
        let Some((journal, profile)) = self.journal.as_ref().zip(self.profile()) else {
            return Ok(());
        };
        let _profile = profile.acquire()?;
        match journal.load()? {
            Some(record) => self.complete_rename(&record),
            None => journal.collect_payloads(),
        }
    }

    fn profile(&self) -> Option<&ComposedLayer> {
        self.layers.iter().find(|layer| layer.persistent())
    }

    pub(super) fn journaled_rename(
        &self,
        from: &str,
        to: &str,
        source: &ComposedLayer,
        destination: &ComposedLayer,
    ) -> Result<(), Error> {
        self.validate_destination(destination.as_ref(), to)?;
        let journal = self
            .journal
            .as_ref()
            .expect("journaled rename has a journal");
        let mut input = StorageReader::new(source.open(from, false)?);
        let record = journal.prepare(
            source.endpoint(from)?,
            destination.endpoint(to)?,
            &mut input,
        )?;
        self.complete_rename(&record)
    }

    fn complete_rename(&self, record: &RenameRecord) -> Result<(), Error> {
        let journal = self.journal.as_ref().expect("recovery has a journal");
        if !record.committed {
            return journal.clear(record);
        }
        let from = self.recorded_layer(&record.from)?;
        let to = self.recorded_layer(&record.to)?;
        let mut payload = journal.payload(record)?;
        let (_, handle) = self.publish(
            &mut payload,
            to.as_ref(),
            &record.to.path,
            Some(record.transaction),
        )?;
        if !record.from.volatile || !journal.resumed(record) {
            from.remove(&record.from.path)?;
        }
        self.profile()
            .expect("recovery has a persistent profile")
            .persist_changes()?;
        journal.clear(record)?;
        if !journal.resumed(record) {
            self.move_binding(&record.from.path, &record.to.path, Some(handle));
        }
        Ok(())
    }

    fn recorded_layer(&self, endpoint: &super::journal::Endpoint) -> Result<&ComposedLayer, Error> {
        let layer = self
            .layers
            .iter()
            .find(|layer| layer.id == endpoint.layer)
            .ok_or_else(|| {
                Error::HistoryConflict(format!(
                    "rename recovery requires layer `{}`",
                    endpoint.layer
                ))
            })?;
        if layer.endpoint(&endpoint.path)?.volatile != endpoint.volatile {
            return Err(Error::HistoryConflict(
                "rename recovery layer lifetime changed".to_owned(),
            ));
        }
        Ok(layer)
    }
}
