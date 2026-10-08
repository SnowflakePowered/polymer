use std::collections::{BTreeMap, BTreeSet};

use crate::Error;

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct WiiState {
    uids: BTreeMap<u64, u32>,
    shared: Vec<[u8; 20]>,
}

impl WiiState {
    pub fn read(value: &serde_json::Value) -> Result<Self, Error> {
        if value.is_null() {
            return Ok(Self {
                uids: BTreeMap::from([(0x0000000100000002, 0x1000)]),
                shared: Vec::new(),
            });
        }
        let state: Self = serde_json::from_value(value.clone())
            .map_err(|error| Error::malformed("Wii composer state", error.to_string()))?;
        if state.uids.get(&0x0000000100000002) != Some(&0x1000)
            || state.uids.values().any(|uid| *uid < 0x1000)
            || state.uids.values().collect::<BTreeSet<_>>().len() != state.uids.len()
            || state.shared.iter().collect::<BTreeSet<_>>().len() != state.shared.len()
        {
            return Err(Error::HistoryConflict(
                "invalid Wii composer allocations".into(),
            ));
        }
        Ok(state)
    }

    pub fn insert_title(&mut self, title: u64) -> Result<(), Error> {
        if !self.uids.contains_key(&title) {
            let next = self
                .uids
                .values()
                .max()
                .copied()
                .unwrap_or(0xfff)
                .checked_add(1)
                .ok_or_else(|| Error::HistoryConflict("Wii UID namespace exhausted".into()))?;
            self.uids.insert(title, next);
        }
        Ok(())
    }

    pub fn shared_path(&mut self, hash: &[u8; 20]) -> Result<String, Error> {
        let id = match self.shared.iter().position(|entry| entry == hash) {
            Some(id) => id,
            None => {
                let id = self.shared.len();
                self.shared.push(*hash);
                id
            }
        };
        let id = u32::try_from(id)
            .map_err(|_| Error::HistoryConflict("Wii shared-content namespace exhausted".into()))?;
        Ok(format!("shared1/{id:08x}.app"))
    }

    pub fn uid_bytes(&self) -> Vec<u8> {
        let mut entries = self.uids.iter().collect::<Vec<_>>();
        entries.sort_by_key(|(_, uid)| *uid);
        entries
            .into_iter()
            .flat_map(|(title, uid)| title.to_be_bytes().into_iter().chain(uid.to_be_bytes()))
            .collect()
    }

    pub fn shared_bytes<'a>(&self, active: impl Iterator<Item = &'a [u8; 20]>) -> Vec<u8> {
        let active = active.collect::<BTreeSet<_>>();
        let mut bytes = Vec::new();
        for (id, hash) in self
            .shared
            .iter()
            .enumerate()
            .filter(|(_, hash)| active.contains(hash))
        {
            bytes.extend_from_slice(format!("{id:08x}").as_bytes());
            bytes.extend_from_slice(hash);
        }
        bytes
    }
}
