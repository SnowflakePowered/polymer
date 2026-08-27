use std::path::PathBuf;

use crate::{Error, Result};

const EPATCH_SIGNATURE: &[u8; 8] = b"Realtech";
const EXTENSION_SIGNATURE: [u8; 4] = [0x51, 0x04, 0xfd, 0x77];
const RTL8761B_PROJECT_ID: u8 = 14;

pub(super) const DOWNLOAD_FRAGMENT_LENGTH: usize = 252;

pub(super) fn load_rtl8761bu_firmware() -> Result<(PathBuf, Vec<u8>)> {
    let path = find_rtl8761bu_firmware().ok_or_else(|| {
        Error::Firmware(
            "RTL8761BU is running from ROM, but rtl8761bu firmware was not found; install the TP-Link/Realtek Bluetooth driver before binding the adapter to WinUSB, or set SNOWGLASS_RTL8761BU_FIRMWARE"
                .into(),
        )
    })?;
    let bytes = std::fs::read(&path)?;
    Ok((path, bytes))
}

fn find_rtl8761bu_firmware() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("SNOWGLASS_RTL8761BU_FIRMWARE") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }

    #[cfg(windows)]
    {
        let system_root = std::env::var_os("SystemRoot")?;
        let repository = PathBuf::from(system_root)
            .join("System32")
            .join("DriverStore")
            .join("FileRepository");
        let filename = "rtl8761b_mp_chip_bt40_fw_asic_rom_patch_new.dat";
        let mut candidates = std::fs::read_dir(repository)
            .ok()?
            .filter_map(std::result::Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .to_ascii_lowercase()
                    .starts_with("rtkfilter.inf_")
            })
            .map(|entry| entry.path().join(filename))
            .filter(|path| path.is_file())
            .collect::<Vec<_>>();
        candidates.sort_by_key(|path| {
            path.metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
        });
        if let Some(path) = candidates.pop() {
            return Some(path);
        }
    }

    #[cfg(not(windows))]
    for path in [
        std::path::Path::new("/lib/firmware/rtl_bt/rtl8761bu_fw.bin"),
        std::path::Path::new("/usr/lib/firmware/rtl_bt/rtl8761bu_fw.bin"),
    ] {
        if path.is_file() {
            return Some(path.to_owned());
        }
    }

    None
}

/// Selects the patch matching `Read ROM Version`, following Linux btrtl.
pub(super) fn prepare_epatch(image: &[u8], rom_version: u8) -> Result<Vec<u8>> {
    if image.len() < 25 || image.get(..8) != Some(EPATCH_SIGNATURE) {
        return Err(Error::Firmware("invalid Realtek epatch signature".into()));
    }
    if !image.ends_with(&EXTENSION_SIGNATURE) {
        return Err(Error::Firmware(
            "Realtek epatch extension signature is missing".into(),
        ));
    }
    if project_id(image) != Some(RTL8761B_PROJECT_ID) {
        return Err(Error::Firmware(
            "firmware payload is not for RTL8761B/BU".into(),
        ));
    }

    let firmware_version: [u8; 4] = image[8..12].try_into().unwrap();
    let count = usize::from(u16::from_le_bytes(image[12..14].try_into().unwrap()));
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

    let chip_ids = 14;
    let patch_lengths = chip_ids + count * 2;
    let patch_offsets = patch_lengths + count * 2;
    let wanted_chip_id = u16::from(rom_version) + 1;
    for index in 0..count {
        let chip_id = le16(image, chip_ids + index * 2)?;
        if chip_id != wanted_chip_id {
            continue;
        }
        let length = usize::from(le16(image, patch_lengths + index * 2)?);
        let offset = usize::try_from(le32(image, patch_offsets + index * 4)?)
            .map_err(|_| Error::Firmware("Realtek patch offset is too large".into()))?;
        let end = offset
            .checked_add(length)
            .ok_or_else(|| Error::Firmware("Realtek patch range overflow".into()))?;
        if length < firmware_version.len() || end > image.len() {
            return Err(Error::Firmware("invalid Realtek patch range".into()));
        }
        let mut patch = image[offset..end].to_vec();
        let version_at = patch.len() - firmware_version.len();
        patch[version_at..].copy_from_slice(&firmware_version);
        return Ok(patch);
    }

    Err(Error::Firmware(format!(
        "firmware has no patch for RTL ROM version {rom_version}"
    )))
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
        if opcode == 0xff {
            return None;
        }
        if length == 0 {
            return None;
        }
        if opcode == 0 && length == 1 {
            return Some(data);
        }
        cursor = cursor.checked_sub(length)?;
    }
    None
}

fn le16(bytes: &[u8], at: usize) -> Result<u16> {
    let value = bytes
        .get(at..at + 2)
        .ok_or_else(|| Error::Firmware("truncated Realtek epatch metadata".into()))?;
    Ok(u16::from_le_bytes(value.try_into().unwrap()))
}

fn le32(bytes: &[u8], at: usize) -> Result<u32> {
    let value = bytes
        .get(at..at + 4)
        .ok_or_else(|| Error::Firmware("truncated Realtek epatch metadata".into()))?;
    Ok(u32::from_le_bytes(value.try_into().unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_rom_patch_and_replaces_version() {
        let mut image = Vec::from(&b"Realtech"[..]);
        image.extend_from_slice(&[0x44, 0x33, 0x22, 0x11]);
        image.extend_from_slice(&1u16.to_le_bytes());
        image.extend_from_slice(&2u16.to_le_bytes());
        image.extend_from_slice(&8u16.to_le_bytes());
        image.extend_from_slice(&24u32.to_le_bytes());
        image.resize(24, 0);
        image.extend_from_slice(&[1, 2, 3, 4, 0, 0, 0, 0]);
        image.extend_from_slice(&[0xff, RTL8761B_PROJECT_ID, 1, 0]);
        image.extend_from_slice(&EXTENSION_SIGNATURE);

        assert_eq!(
            prepare_epatch(&image, 1).unwrap(),
            [1, 2, 3, 4, 0x44, 0x33, 0x22, 0x11]
        );
    }

    #[test]
    fn rejects_wrong_project() {
        let mut image = vec![0; 25];
        image[..8].copy_from_slice(EPATCH_SIGNATURE);
        let end = image.len();
        image[end - 8..end - 4].copy_from_slice(&[0xff, 3, 1, 0]);
        image[end - 4..].copy_from_slice(&EXTENSION_SIGNATURE);
        assert!(prepare_epatch(&image, 0).is_err());
    }
}
