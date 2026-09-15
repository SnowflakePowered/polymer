use std::io::Read;

use orbis_unpkg::Psf;

use crate::Error;

/// Bounded PSF decoding shared by Sony save metadata and directory depots.
pub(crate) struct SonyMetadata;

impl SonyMetadata {
    pub const SFO_PATHS: [&str; 2] = ["PARAM.SFO", "sce_sys/param.sfo"];
    pub const ICON_PATHS: [&str; 2] = ["ICON0.PNG", "sce_sys/icon0.png"];

    pub fn title(sfo: &Psf) -> Option<&str> {
        ["TITLE_01", "TITLE"]
            .into_iter()
            .filter_map(|key| sfo.get_string(key))
            .chain((0..20).filter_map(|index| sfo.get_string(&format!("TITLE_{index:02}"))))
            .find(|name| !name.trim().is_empty())
    }

    pub fn read(source: impl Read, path: &str) -> Result<Psf, Error> {
        const MAX_BYTES: u64 = 1024 * 1024;
        let mut bytes = Vec::new();
        source
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|source| Error::Io {
                path: path.to_owned(),
                source,
            })?;
        if bytes.len() as u64 > MAX_BYTES || bytes.len() < 20 {
            return Err(Error::malformed(path, "invalid PSF size"));
        }
        // The parser allocates from this count before validating its index table.
        let count = u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as usize;
        if count > (bytes.len() - 20) / 16 {
            return Err(Error::malformed(path, "truncated PSF index table"));
        }
        Psf::parse(&bytes).map_err(|error| Error::malformed(path, error.to_string()))
    }
}

#[cfg(test)]
impl SonyMetadata {
    pub(crate) fn fixture(values: &[(&str, &str)]) -> Vec<u8> {
        let mut keys = Vec::new();
        let mut data = Vec::new();
        let mut entries = Vec::new();
        for (key, value) in values {
            entries.extend_from_slice(&(keys.len() as u16).to_le_bytes());
            entries.extend_from_slice(&0x0204_u16.to_le_bytes());
            entries.extend_from_slice(&((value.len() + 1) as u32).to_le_bytes());
            entries.extend_from_slice(&((value.len() + 1) as u32).to_le_bytes());
            entries.extend_from_slice(&(data.len() as u32).to_le_bytes());
            keys.extend_from_slice(key.as_bytes());
            keys.push(0);
            data.extend_from_slice(value.as_bytes());
            data.push(0);
        }
        let key_offset = 20 + entries.len();
        let data_offset = key_offset + keys.len();
        let mut bytes = b"\0PSF".to_vec();
        bytes.extend_from_slice(&0x101_u32.to_le_bytes());
        bytes.extend_from_slice(&(key_offset as u32).to_le_bytes());
        bytes.extend_from_slice(&(data_offset as u32).to_le_bytes());
        bytes.extend_from_slice(&(values.len() as u32).to_le_bytes());
        bytes.extend(entries);
        bytes.extend(keys);
        bytes.extend(data);
        bytes
    }
}
