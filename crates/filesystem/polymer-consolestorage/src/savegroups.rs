use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::LazyLock;

use serde::Deserialize;

use crate::savegame::id::{DreamcastProductId, SonyProductCode};
use crate::savegame::SaveDepotId;

pub(crate) const TARGET_CAPACITY_PERCENT: u32 = 80;

#[derive(Deserialize)]
struct SaveGroupFile {
    savegroup: Vec<SaveGroup>,
}

#[derive(Deserialize)]
struct SaveGroup {
    ids: Vec<String>,
}

pub(crate) struct SaveGroups {
    lookup: LazyLock<HashMap<u64, Vec<u64>>>,
}

impl SaveGroups {
    const fn new(lookup: LazyLock<HashMap<u64, Vec<u64>>>) -> Self {
        Self { lookup }
    }

    fn build(source: &str, depot_ids: fn(&str) -> Vec<u64>) -> HashMap<u64, Vec<u64>> {
        let file: SaveGroupFile = toml::from_str(source).expect("deserialize save groups");
        let mut lookup = HashMap::<u64, BTreeSet<u64>>::new();
        for group in file.savegroup.iter().filter(|group| group.ids.len() >= 2) {
            let members = group
                .ids
                .iter()
                .flat_map(|serial| depot_ids(serial))
                .collect::<BTreeSet<_>>();
            for id in &members {
                lookup.entry(*id).or_default().extend(&members);
            }
        }
        lookup
            .into_iter()
            .map(|(id, members)| (id, members.into_iter().collect()))
            .collect()
    }

    fn sony_depot_ids(serial: &str) -> Vec<u64> {
        if serial.len() != 10 || !serial.is_ascii() {
            return Vec::new();
        }
        // The group files contain disc serials, while the authoritative native save name
        // prefixes that serial with the regional memory-card key code. Generate every
        // recognized key-code form so lookup remains based on the native SaveDepotId.
        [b"BA", b"BE", b"BI", b"BK"]
            .into_iter()
            .map(|key| {
                let mut native_id = [0; 12];
                native_id[..2].copy_from_slice(key);
                native_id[2..].copy_from_slice(serial.as_bytes());
                let id: SaveDepotId = SonyProductCode::new(&native_id)
                    .expect("a save-group serial forms a product code")
                    .into();
                id.0
            })
            .collect()
    }

    fn dreamcast_depot_ids(product: &str) -> Vec<u64> {
        DreamcastProductId::new(product.as_bytes())
            .map(|product| SaveDepotId::from(product).0)
            .into_iter()
            .collect()
    }

    pub(crate) fn members(&self, primary: &[SaveDepotId]) -> HashSet<u64> {
        primary
            .iter()
            .filter_map(|id| self.lookup.get(&id.0))
            .flat_map(|members| members.iter().copied())
            .collect()
    }
}

pub(crate) static PS1_SAVE_GROUPS: SaveGroups = SaveGroups::new(LazyLock::new(|| {
    SaveGroups::build(
        include_str!("impls/ps1/savegroups.toml"),
        SaveGroups::sony_depot_ids,
    )
}));
pub(crate) static PS2_SAVE_GROUPS: SaveGroups = SaveGroups::new(LazyLock::new(|| {
    SaveGroups::build(
        include_str!("impls/ps2/savegroups.toml"),
        SaveGroups::sony_depot_ids,
    )
}));
pub(crate) static DREAMCAST_SAVE_GROUPS: SaveGroups = SaveGroups::new(LazyLock::new(|| {
    SaveGroups::build(
        include_str!("impls/dreamcast/savegroups.toml"),
        SaveGroups::dreamcast_depot_ids,
    )
}));
