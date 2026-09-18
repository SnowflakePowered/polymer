pub mod depots;
mod index;
pub mod nand;
pub mod save_repo;
pub mod ticket;

pub use nand::WiiNand;
pub use save_repo::WiiSaveRepo;

use crate::savegame::SaveDepotId;

pub(crate) const SYSTEM_TITLE_HIGH: u32 = 0x0000_0001;

pub(crate) fn title_path(id: u64) -> String {
    format!("title/{:08x}/{:08x}", id >> 32, id as u32)
}

pub(crate) fn data_path(id: u64) -> String {
    format!("{}/data", title_path(id))
}

pub(crate) fn title_id_from_path(path: &str) -> Option<u64> {
    let mut parts = path.split('/');
    (parts.next()? == "title").then_some(())?;
    let high = u32::from_str_radix(parts.next()?, 16).ok()?;
    let low = u32::from_str_radix(parts.next()?, 16).ok()?;
    Some((u64::from(high) << 32) | u64::from(low))
}

pub(crate) fn save_id_from_path(path: &str) -> Option<SaveDepotId> {
    let id = title_id_from_path(path)?;
    let component = path.split('/').nth(3)?;
    (component == "data" && (id >> 32) as u32 != SYSTEM_TITLE_HIGH).then_some(SaveDepotId(id))
}

pub(crate) fn is_user_data_path(path: &str) -> bool {
    save_id_from_path(path).is_some()
}
