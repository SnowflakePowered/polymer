use std::io::Read;

use orbis_unpkg::Psf;

use crate::Error;

/// Bounded PSF decoding shared by Sony save metadata and directory depots.
pub(crate) struct SonyMetadata;

impl SonyMetadata {
    pub const SFO_PATHS: [&str; 2] = ["PARAM.SFO", "sce_sys/param.sfo"];
    pub const ICON_PATHS: [&str; 2] = ["ICON0.PNG", "sce_sys/icon0.png"];

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
