use crate::savegame::SaveDepotId;

/// Azahar uses fixed, zero-valued console and SD identities.
pub(super) struct Layout;

impl Layout {
    pub const SD: &str =
        "sdmc/Nintendo 3DS/00000000000000000000000000000000/00000000000000000000000000000000";
    pub const NAND_DATA: &str = "nand/data/00000000000000000000000000000000";

    pub fn valid_path(path: &str) -> bool {
        path.split('/').all(|part| {
            !part.is_empty()
                && !part.ends_with(['.', ' '])
                && !part.eq_ignore_ascii_case(".git")
                && !part.contains(['\\', ':', '\0', '<', '>', '"', '|', '?', '*'])
        })
    }

    pub fn profile_path(path: &str) -> bool {
        let nand_data = path
            .strip_prefix(Self::NAND_DATA)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'));
        let sd_data = path == "sdmc" || path.starts_with("sdmc/");
        let titles = format!("{}/title", Self::SD);
        let installed_tree = path
            .strip_prefix(&titles)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'));
        nand_data || (sd_data && !installed_tree)
    }
}

/// SD extdata IDs occupy the zero high-word namespace, distinct from title IDs.
/// Shared extdata is profile data, not an independently owned game save.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Archive {
    Title(u64),
    Extdata(u32),
}

impl Archive {
    pub fn from_id(id: SaveDepotId) -> Option<Self> {
        match id.0 >> 32 {
            0 if id.0 != 0 => Some(Self::Extdata(id.0 as u32)),
            high if high >> 16 == 4 => Some(Self::Title(id.0)),
            _ => None,
        }
    }

    pub fn from_path(path: &str) -> Option<Self> {
        if !Layout::valid_path(path) {
            return None;
        }
        let relative = path.strip_prefix(Layout::SD)?.strip_prefix('/')?;
        let mut parts = relative.split('/');
        let kind = parts.next()?;
        let high = parts.next()?;
        let low = parts.next()?;
        if [high, low]
            .iter()
            .any(|part| part.len() != 8 || !part.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return None;
        }
        let high = u32::from_str_radix(high, 16).ok()?;
        let low = u32::from_str_radix(low, 16).ok()?;
        match (kind, high) {
            ("title", high) if high >> 16 == 4 && parts.next()? == "data" => {
                Some(Self::Title((u64::from(high) << 32) | u64::from(low)))
            }
            ("extdata", 0) if low != 0 => Some(Self::Extdata(low)),
            _ => None,
        }
    }

    pub fn id(self) -> SaveDepotId {
        SaveDepotId(match self {
            Self::Title(id) => id,
            Self::Extdata(low) => u64::from(low),
        })
    }

    pub fn root(self) -> String {
        match self {
            Self::Title(id) => format!(
                "{}/title/{:08x}/{:08x}/data",
                Layout::SD,
                id >> 32,
                id as u32
            ),
            Self::Extdata(low) => format!("{}/extdata/00000000/{low:08x}", Layout::SD),
        }
    }
}
