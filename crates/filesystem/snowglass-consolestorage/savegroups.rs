use std::collections::HashSet;

use crate::savegame::SaveDepotId;

pub(crate) const TARGET_CAPACITY_PERCENT: u32 = 80;

pub(crate) struct SaveGroups {
    lookup: &'static phf::Map<u64, &'static [u64]>,
}

impl SaveGroups {
    const fn new(lookup: &'static phf::Map<u64, &'static [u64]>) -> Self {
        Self { lookup }
    }

    pub(crate) fn members(&self, primary: &[SaveDepotId]) -> HashSet<u64> {
        primary
            .iter()
            .filter_map(|id| self.lookup.get(&id.0))
            .flat_map(|members| members.iter().copied())
            .collect()
    }
}

include!(concat!(env!("OUT_DIR"), "/savegroups.rs"));

pub(crate) static PS1_SAVE_GROUPS: SaveGroups = SaveGroups::new(&PS1_SAVE_GROUPS_MAP);
pub(crate) static PS2_SAVE_GROUPS: SaveGroups = SaveGroups::new(&PS2_SAVE_GROUPS_MAP);
pub(crate) static DREAMCAST_SAVE_GROUPS: SaveGroups = SaveGroups::new(&DREAMCAST_SAVE_GROUPS_MAP);
