use super::{FirmwareDescription, FirmwareError};
use std::{collections::HashMap, path::Path};

/// Retains the source handle from recognition through installation.
pub(super) struct BiosFile {
    pub file: std::fs::File,
    pub description: FirmwareDescription,
    pub digest: String,
    pub definition: consolespec::machine::Bios,
}

impl BiosFile {
    pub fn read(path: &Path) -> Result<Option<Self>, FirmwareError> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // Allow other readers while preventing writes or replacement during import.
            options.share_mode(1);
        }
        let mut file = options.open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(FirmwareError::Invalid(
                "BIOS input must be a regular file".into(),
            ));
        }
        let byte_length = metadata.len();
        let mut hashes = HashMap::new();
        for machine in consolespec::MachineSpec::all() {
            for bios in machine.bios() {
                if Self::matches(bios, &mut file, &mut hashes)? {
                    return Ok(Some(Self {
                        digest: Self::calculate(&mut file, "blake3")?,
                        description: FirmwareDescription {
                            platform: machine.id().into(),
                            label: bios
                                .names()
                                .next()
                                .ok_or_else(|| {
                                    FirmwareError::Invalid("consolespec BIOS has no names".into())
                                })?
                                .into(),
                            names: bios.names().map(str::to_owned).collect(),
                            byte_length,
                        },
                        file,
                        definition: bios,
                    }));
                }
            }
        }
        Ok(None)
    }
    fn matches(
        bios: consolespec::machine::Bios,
        file: &mut std::fs::File,
        hashes: &mut HashMap<&'static str, String>,
    ) -> Result<bool, FirmwareError> {
        for (algorithm, expected) in [("sha256", bios.sha256()), ("md5", bios.md5())] {
            let Some(expected) = expected else { continue };
            let actual = match hashes.entry(algorithm) {
                std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(Self::calculate(file, algorithm)?)
                }
            };
            if actual.eq_ignore_ascii_case(expected) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
