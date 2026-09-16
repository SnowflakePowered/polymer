use super::{FirmwareDescription, FirmwareError};
use md5::{Digest, Md5};
use sha2::Sha256;
use std::path::Path;

/// A single immutable snapshot used for both hash recognition and installation.
pub(super) struct BiosFile {
    pub contents: Vec<u8>,
    pub description: FirmwareDescription,
    pub digest: String,
}

impl BiosFile {
    pub fn read(path: &Path) -> Result<Option<Self>, FirmwareError> {
        let mut file = std::fs::File::open(path)?;
        if !file.metadata()?.is_file() {
            return Err(FirmwareError::Invalid(
                "BIOS input must be a regular file".into(),
            ));
        }
        let mut contents = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut contents)?;
        let md5: String = Md5::digest(&contents)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let sha256: String = Sha256::digest(&contents)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        for machine in consolespec::MachineSpec::all() {
            for bios in machine.bios() {
                if bios.sha256().any(|hash| hash.eq_ignore_ascii_case(&sha256))
                    || bios.md5().any(|hash| hash.eq_ignore_ascii_case(&md5))
                {
                    return Ok(Some(Self {
                        digest: blake3::hash(&contents).to_hex().to_string(),
                        description: FirmwareDescription {
                            platform: machine.id().into(),
                            firmware_type: "bios".into(),
                            label: bios.name().into(),
                            byte_length: contents.len() as u64,
                        },
                        contents,
                    }));
                }
            }
        }
        Ok(None)
    }
}
