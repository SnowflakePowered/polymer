use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[allow(dead_code)]
#[path = "savegame/id.rs"]
mod save_depot_id;

#[derive(Deserialize)]
struct SaveGroups {
    savegroup: Vec<SaveGroup>,
}

#[derive(Deserialize)]
struct SaveGroup {
    ids: Vec<String>,
}

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let ps1 = manifest.join("impls/ps1/savegroups.toml");
    let ps2 = manifest.join("impls/ps2/savegroups.toml");
    let dreamcast = manifest.join("impls/dreamcast/savegroups.toml");
    println!("cargo:rerun-if-changed={}", ps1.display());
    println!("cargo:rerun-if-changed={}", ps2.display());
    println!("cargo:rerun-if-changed={}", dreamcast.display());

    let mut generated = String::new();
    generate_map(&mut generated, "PS1_SAVE_GROUPS_MAP", &ps1, sony_depot_ids);
    generate_map(&mut generated, "PS2_SAVE_GROUPS_MAP", &ps2, sony_depot_ids);
    generate_map(
        &mut generated,
        "DREAMCAST_SAVE_GROUPS_MAP",
        &dreamcast,
        dreamcast_depot_ids,
    );
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("build output directory"));
    fs::write(output.join("savegroups.rs"), generated).expect("write generated save groups");
}

fn generate_map(output: &mut String, name: &str, path: &Path, depot_ids: fn(&str) -> Vec<u64>) {
    let source = fs::read_to_string(path).expect("read save groups");
    let groups: SaveGroups = toml::from_str(&source).expect("deserialize save groups");
    let mut lookup = BTreeMap::<u64, BTreeSet<u64>>::new();
    for group in groups.savegroup {
        if group.ids.len() < 2 {
            continue;
        }
        let members = group
            .ids
            .iter()
            .flat_map(|serial| depot_ids(serial))
            .collect::<BTreeSet<_>>();
        for id in &members {
            lookup.entry(*id).or_default().extend(&members);
        }
    }

    let mut map = phf_codegen::Map::<u64>::new();
    for (id, members) in lookup {
        let mut value = String::from("&[");
        for member in members {
            write!(value, "{member}u64,").expect("write generated group member");
        }
        value.push(']');
        map.entry(id, value);
    }
    writeln!(
        output,
        "static {name}: ::phf::Map<u64, &'static [u64]> = {};",
        map.build()
    )
    .expect("write generated save-group map");
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
            let id: save_depot_id::SaveDepotId = save_depot_id::SonyProductCode::new(&native_id)
                .expect("a save-group serial forms a product code")
                .into();
            id.0
        })
        .collect()
}

fn dreamcast_depot_ids(product: &str) -> Vec<u64> {
    save_depot_id::DreamcastProductId::new(product.as_bytes())
        .map(|product| {
            let id: save_depot_id::SaveDepotId = product.into();
            id.0
        })
        .into_iter()
        .collect()
}
