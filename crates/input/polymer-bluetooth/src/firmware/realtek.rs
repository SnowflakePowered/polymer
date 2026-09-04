use std::mem;

use zerocopy::{
    FromBytes, Immutable, KnownLayout, Unaligned,
    byteorder::little_endian::{U16, U32},
};

use crate::{Error, Result};

use super::{FirmwareIdentity, FirmwareLoader, PreparedFirmware};

const EPATCH_SIGNATURE: &[u8; 8] = b"Realtech";
const EPATCH_V2_SIGNATURE: &[u8; 8] = b"RTBTCore";
const EXTENSION_SIGNATURE: [u8; 4] = [0x51, 0x04, 0xfd, 0x77];

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct EpatchHeader {
    signature: [u8; 8],
    firmware_version: [u8; 4],
    patch_count: U16,
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct EpatchV2Header {
    signature: [u8; 8],
    version_and_reserved: [u8; 8],
    section_count: U32,
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct V2SectionHeader {
    opcode: U32,
    length: U32,
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct V2SubsectionListHeader {
    count: U16,
    reserved: U16,
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct V2SubsectionHeader {
    eco: u8,
    priority: u8,
    key_id: u8,
    reserved: u8,
    length: U32,
}

pub const REALTEK_FAMILIES: &[&str] = &[
    "RTL8723A",
    "RTL8723B",
    "RTL8723D",
    "RTL8761A",
    "RTL8761BU",
    "RTL8821A",
    "RTL8821C",
    "RTL8822B",
    "RTL8822CU",
    "RTL8851BU",
    "RTL8852AU",
    "RTL8852BU",
    "RTL8852CU",
    "RTL8852BTU",
    "RTL8922AU",
];

#[derive(Clone, Copy, Debug)]
struct Asset {
    family: &'static str,
    firmware: &'static [u8],
    config: &'static [u8],
    project_id: Option<u8>,
    raw: bool,
}

const CONFIG_8723BS: &[u8] = include_bytes!("assets/rtl8723bs_config.bin");
const CONFIG_8761BU: &[u8] = include_bytes!("assets/rtl8761bu_config.bin");
const CONFIG_8821C: &[u8] = include_bytes!("assets/rtl8821c_config.bin");
const CONFIG_8822B: &[u8] = include_bytes!("assets/rtl8822b_config.bin");
const CONFIG_8922AU: &[u8] = include_bytes!("assets/rtl8922au_config.bin");

const ASSETS: &[Asset] = &[
    Asset {
        family: "rtl8723a",
        firmware: include_bytes!("assets/rtl8723a_fw.bin"),
        config: &[],
        project_id: None,
        raw: true,
    },
    Asset {
        family: "rtl8723b",
        firmware: include_bytes!("assets/rtl8723b_fw.bin"),
        config: CONFIG_8723BS,
        project_id: Some(1),
        raw: false,
    },
    Asset {
        family: "rtl8723d",
        firmware: include_bytes!("assets/rtl8723d_fw.bin"),
        config: CONFIG_8821C,
        project_id: Some(9),
        raw: false,
    },
    Asset {
        family: "rtl8761a",
        firmware: include_bytes!("assets/rtl8761a_fw.bin"),
        config: CONFIG_8761BU,
        project_id: Some(3),
        raw: false,
    },
    Asset {
        family: "rtl8761bu",
        firmware: include_bytes!("assets/rtl8761bu_fw.bin"),
        config: CONFIG_8761BU,
        project_id: Some(14),
        raw: false,
    },
    Asset {
        family: "rtl8821a",
        firmware: include_bytes!("assets/rtl8821a_fw.bin"),
        config: CONFIG_8821C,
        project_id: Some(2),
        raw: false,
    },
    Asset {
        family: "rtl8821c",
        firmware: include_bytes!("assets/rtl8821c_fw.bin"),
        config: CONFIG_8821C,
        project_id: Some(10),
        raw: false,
    },
    Asset {
        family: "rtl8822b",
        firmware: include_bytes!("assets/rtl8822b_fw.bin"),
        config: CONFIG_8822B,
        project_id: Some(8),
        raw: false,
    },
    Asset {
        family: "rtl8822cu",
        firmware: include_bytes!("assets/rtl8822cu_fw.bin"),
        config: CONFIG_8761BU,
        project_id: Some(13),
        raw: false,
    },
    Asset {
        family: "rtl8851bu",
        firmware: include_bytes!("assets/rtl8851bu_fw.bin"),
        config: CONFIG_8761BU,
        project_id: Some(36),
        raw: false,
    },
    Asset {
        family: "rtl8852au",
        firmware: include_bytes!("assets/rtl8852au_fw.bin"),
        config: CONFIG_8761BU,
        project_id: Some(18),
        raw: false,
    },
    Asset {
        family: "rtl8852bu",
        firmware: include_bytes!("assets/rtl8852bu_fw.bin"),
        config: CONFIG_8761BU,
        project_id: Some(20),
        raw: false,
    },
    Asset {
        family: "rtl8852cu",
        firmware: include_bytes!("assets/rtl8852cu_fw.bin"),
        config: CONFIG_8761BU,
        project_id: Some(25),
        raw: false,
    },
    Asset {
        family: "rtl8852btu",
        firmware: include_bytes!("assets/rtl8852btu_fw.bin"),
        config: CONFIG_8761BU,
        project_id: Some(47),
        raw: false,
    },
    Asset {
        family: "rtl8922au",
        firmware: include_bytes!("assets/rtl8922au_fw.bin"),
        config: CONFIG_8922AU,
        project_id: Some(44),
        raw: false,
    },
];

#[derive(Clone, Copy, Debug, Default)]
pub struct EmbeddedRealtek;

impl FirmwareLoader for EmbeddedRealtek {
    fn name(&self) -> &'static str {
        "embedded-realtek"
    }

    fn recognizes(&self, identity: &FirmwareIdentity) -> bool {
        identity.manufacturer == 93 && asset_name(identity).is_some()
    }

    fn requires_security_key(&self, identity: &FirmwareIdentity) -> bool {
        asset_name(identity)
            .and_then(|name| ASSETS.iter().find(|asset| asset.family == name))
            .is_some_and(|asset| asset.firmware.starts_with(EPATCH_V2_SIGNATURE))
    }

    fn prepare(
        &self,
        identity: &FirmwareIdentity,
        rom_version: Option<u8>,
        security_key_id: Option<u8>,
    ) -> Result<Option<PreparedFirmware>> {
        let Some(name) = asset_name(identity) else {
            return Ok(None);
        };
        let asset = ASSETS
            .iter()
            .find(|asset| asset.family == name)
            .ok_or_else(|| Error::Firmware(format!("embedded Realtek asset {name} is missing")))?;
        let bytes = if asset.raw {
            if asset.firmware.starts_with(EPATCH_SIGNATURE) {
                return Err(Error::Firmware(
                    "RTL8723A firmware unexpectedly uses epatch format".into(),
                ));
            }
            asset.firmware.to_vec()
        } else if asset.firmware.starts_with(EPATCH_V2_SIGNATURE) {
            prepare_epatch_v2(
                asset,
                rom_version.ok_or(Error::InvalidPacket("missing Realtek ROM version"))?,
                security_key_id.unwrap_or(0),
            )?
        } else {
            prepare_epatch(
                asset,
                rom_version.ok_or(Error::InvalidPacket("missing Realtek ROM version"))?,
            )?
        };
        Ok(Some(PreparedFirmware {
            loader: self.name(),
            family: asset.family,
            bytes,
        }))
    }
}

fn asset_name(identity: &FirmwareIdentity) -> Option<&'static str> {
    match (
        identity.lmp_subversion,
        identity.hci_revision,
        identity.hci_version,
    ) {
        (0x1200, 0x000b, 0x06) => Some("rtl8723a"),
        (0x8723, 0x000b, 0x06) => Some("rtl8723b"),
        (0x8723, 0x000d, 0x08) => Some("rtl8723d"),
        (0x8761, 0x000a, 0x06) => Some("rtl8761a"),
        (0x8761, 0x000b, 0x0a) => Some("rtl8761bu"),
        (0x8821, 0x000a, 0x06) => Some("rtl8821a"),
        (0x8821, 0x000c, 0x08) => Some("rtl8821c"),
        (0x8822, 0x000b, 0x07) => Some("rtl8822b"),
        (0x8822, 0x000c, 0x0a) => Some("rtl8822cu"),
        (0x8851, 0x000b, 0x0c) => Some("rtl8851bu"),
        (0x8852, 0x000a, 0x0b) => Some("rtl8852au"),
        (0x8852, 0x000b, 0x0b) => Some("rtl8852bu"),
        (0x8852, 0x000c, 0x0c) => Some("rtl8852cu"),
        (0x8852, 0x0087, 0x0c) => Some("rtl8852btu"),
        (0x8922, 0x000a, 0x0c) => Some("rtl8922au"),
        _ => None,
    }
}

fn prepare_epatch(asset: &Asset, rom_version: u8) -> Result<Vec<u8>> {
    let image = asset.firmware;
    if image.len() < 25 {
        return Err(Error::Firmware("Realtek firmware is truncated".into()));
    }
    let header = wire::<EpatchHeader>(image)?;
    if &header.signature != EPATCH_SIGNATURE {
        return Err(Error::Firmware("invalid Realtek epatch signature".into()));
    }
    if !image.ends_with(&EXTENSION_SIGNATURE) {
        return Err(Error::Firmware(
            "Realtek epatch extension signature is missing".into(),
        ));
    }
    if project_id(image) != asset.project_id {
        return Err(Error::Firmware(format!(
            "wrong Realtek project id for {}",
            asset.family
        )));
    }

    let firmware_version = header.firmware_version;
    let count = usize::from(header.patch_count.get());
    let metadata_end = 14usize
        .checked_add(
            count
                .checked_mul(8)
                .ok_or_else(|| Error::Firmware("Realtek epatch metadata length overflow".into()))?,
        )
        .ok_or_else(|| Error::Firmware("Realtek epatch metadata length overflow".into()))?;
    if metadata_end > image.len() {
        return Err(Error::Firmware("truncated Realtek epatch metadata".into()));
    }

    let lengths = 14 + count * 2;
    let offsets = lengths + count * 2;
    for index in 0..count {
        if wire_at::<U16>(image, 14 + index * 2)?.get() != u16::from(rom_version) + 1 {
            continue;
        }
        let length = usize::from(wire_at::<U16>(image, lengths + index * 2)?.get());
        let offset = usize::try_from(wire_at::<U32>(image, offsets + index * 4)?.get())
            .map_err(|_| Error::Firmware("Realtek patch offset is too large".into()))?;
        let end = offset
            .checked_add(length)
            .ok_or_else(|| Error::Firmware("Realtek patch range overflow".into()))?;
        if length < 4 || end > image.len() {
            return Err(Error::Firmware("invalid Realtek patch range".into()));
        }
        let mut patch = image[offset..end].to_vec();
        patch[length - 4..].copy_from_slice(&firmware_version);
        patch.extend_from_slice(asset.config);
        return Ok(patch);
    }
    Err(Error::Firmware(format!(
        "{} has no patch for ROM version {rom_version}",
        asset.family
    )))
}

fn prepare_epatch_v2(asset: &Asset, rom_version: u8, security_key_id: u8) -> Result<Vec<u8>> {
    let image = asset.firmware;
    if image.len() < 27 {
        return Err(Error::Firmware("invalid Realtek epatch v2 header".into()));
    }
    let header = wire::<EpatchV2Header>(image)?;
    if &header.signature != EPATCH_V2_SIGNATURE {
        return Err(Error::Firmware("invalid Realtek epatch v2 header".into()));
    }
    let section_count = usize::try_from(header.section_count.get())
        .map_err(|_| Error::Firmware("Realtek v2 section count is too large".into()))?;
    let limit = image
        .len()
        .checked_sub(7)
        .ok_or_else(|| Error::Firmware("truncated Realtek v2 trailer".into()))?;
    let mut cursor = mem::size_of::<EpatchV2Header>();
    let mut selected = Vec::<(u8, usize, &[u8])>::new();
    let mut order = 0usize;
    for _ in 0..section_count {
        let section_header = wire_at::<V2SectionHeader>(image, cursor)?;
        let opcode = section_header.opcode.get();
        let section_len = usize::try_from(section_header.length.get())
            .map_err(|_| Error::Firmware("Realtek v2 section is too large".into()))?;
        cursor = cursor
            .checked_add(mem::size_of::<V2SectionHeader>())
            .ok_or_else(|| Error::Firmware("Realtek v2 section overflow".into()))?;
        let end = cursor
            .checked_add(section_len)
            .ok_or_else(|| Error::Firmware("Realtek v2 section overflow".into()))?;
        if end > limit {
            return Err(Error::Firmware("truncated Realtek v2 section".into()));
        }
        if matches!(opcode, 1..=3) {
            let section = &image[cursor..end];
            let list_header = wire::<V2SubsectionListHeader>(section)
                .map_err(|_| Error::Firmware("truncated Realtek v2 subsection header".into()))?;
            let count = usize::from(list_header.count.get());
            let mut at = mem::size_of::<V2SubsectionListHeader>();
            for _ in 0..count {
                let subsection = wire_at::<V2SubsectionHeader>(section, at)
                    .map_err(|_| Error::Firmware("truncated Realtek v2 subsection".into()))?;
                let length = usize::try_from(subsection.length.get())
                    .map_err(|_| Error::Firmware("Realtek v2 subsection is too large".into()))?;
                at += mem::size_of::<V2SubsectionHeader>();
                let data = section.get(at..at + length).ok_or_else(|| {
                    Error::Firmware("truncated Realtek v2 subsection data".into())
                })?;
                if subsection.eco == rom_version.wrapping_add(1)
                    && (opcode != 3
                        || (security_key_id != 0 && subsection.key_id == security_key_id))
                {
                    selected.push((subsection.priority, order, data));
                    order += 1;
                }
                at += length;
            }
        }
        cursor = end;
    }
    if selected.is_empty() {
        return Err(Error::Firmware(format!(
            "{} has no v2 patch for ROM version {rom_version}",
            asset.family
        )));
    }
    selected.sort_by_key(|(priority, order, _)| (*priority, *order));
    let total = selected
        .iter()
        .try_fold(asset.config.len(), |total, (_, _, data)| {
            total.checked_add(data.len())
        })
        .ok_or_else(|| Error::Firmware("Realtek v2 patch size overflow".into()))?;
    let mut patch = Vec::with_capacity(total);
    for (_, _, data) in selected {
        patch.extend_from_slice(data);
    }
    patch.extend_from_slice(asset.config);
    Ok(patch)
}

fn project_id(image: &[u8]) -> Option<u8> {
    let mut cursor = image.len().checked_sub(EXTENSION_SIGNATURE.len())?;
    while cursor >= 3 {
        cursor -= 1;
        let opcode = image[cursor];
        cursor -= 1;
        let length = usize::from(image[cursor]);
        cursor -= 1;
        let data = image[cursor];
        if opcode == 0xff || length == 0 {
            return None;
        }
        if opcode == 0 && length == 1 {
            return Some(data);
        }
        cursor = cursor.checked_sub(length)?;
    }
    None
}

fn wire<T>(bytes: &[u8]) -> Result<&T>
where
    T: FromBytes + KnownLayout + Immutable + Unaligned,
{
    T::ref_from_prefix(bytes)
        .map(|(value, _)| value)
        .map_err(|_| Error::Firmware("truncated Realtek epatch metadata".into()))
}

fn wire_at<T>(bytes: &[u8], at: usize) -> Result<&T>
where
    T: FromBytes + KnownLayout + Immutable + Unaligned,
{
    let bytes = bytes
        .get(at..)
        .ok_or_else(|| Error::Firmware("truncated Realtek epatch metadata".into()))?;
    wire(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_supported_family() {
        let tuples = [
            (0x1200, 0xb, 0x6),
            (0x8723, 0xb, 0x6),
            (0x8723, 0xd, 0x8),
            (0x8761, 0xa, 0x6),
            (0x8761, 0xb, 0xa),
            (0x8821, 0xa, 0x6),
            (0x8821, 0xc, 0x8),
            (0x8822, 0xb, 0x7),
            (0x8822, 0xc, 0xa),
            (0x8851, 0xb, 0xc),
            (0x8852, 0xa, 0xb),
            (0x8852, 0xb, 0xb),
            (0x8852, 0xc, 0xc),
            (0x8852, 0x87, 0xc),
            (0x8922, 0xa, 0xc),
        ];
        for (lmp_subversion, hci_revision, hci_version) in tuples {
            assert!(
                asset_name(&FirmwareIdentity {
                    vendor_id: 0x0bda,
                    product_id: 0,
                    hci_version,
                    hci_revision,
                    manufacturer: 93,
                    lmp_subversion,
                })
                .is_some()
            );
        }
    }

    #[test]
    fn embedded_epatches_parse_for_at_least_one_rom() {
        for asset in ASSETS.iter().filter(|asset| !asset.raw) {
            let parses = (0..=0xff).any(|rom| {
                if asset.firmware.starts_with(EPATCH_V2_SIGNATURE) {
                    (0..=0xff).any(|key| prepare_epatch_v2(asset, rom, key).is_ok())
                } else {
                    prepare_epatch(asset, rom).is_ok()
                }
            });
            assert!(parses, "{}", asset.family);
        }
    }
}
