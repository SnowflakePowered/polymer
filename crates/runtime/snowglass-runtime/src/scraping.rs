use gumshoe::{
    GeneralIdentifier, IdentifyPackage, PlatformIdentifier, binary::NormalizationOptions,
};
use snowglass_consolestorage::BatterySaveRepo;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Immutable package identity produced by Gumshoe and exposed to the UI realm.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrapedPackage {
    pub path: PathBuf,
    pub name: String,
    pub platform: String,
    pub serial: Option<String>,
    pub title_id: Option<String>,
    pub binary_digest: Option<String>,
}

impl ScrapedPackage {
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, ScrapingError> {
        let requested = path.as_ref();
        let path = requested
            .canonicalize()
            .map_err(|source| ScrapingError::Open {
                path: requested.to_path_buf(),
                source,
            })?;
        let package =
            GeneralIdentifier
                .identify_path(&path)
                .map_err(|source| ScrapingError::Identify {
                    path: path.clone(),
                    reason: source.to_string(),
                })?;
        let platform = consolespec_platform(package.platform()).ok_or_else(|| {
            ScrapingError::UnsupportedPlatform {
                path: path.clone(),
                platform: format!("{:?}", package.platform()),
            }
        })?;
        let serial = package.serial().map(str::to_owned);
        let title_id = package.titleid();
        let binary_digest = if BatterySaveRepo::supports(platform)
            && title_id.as_deref().is_none_or(str::is_empty)
            && serial.as_deref().is_none_or(str::is_empty)
        {
            Some(Self::binary_digest(package.as_ref(), &path)?)
        } else {
            None
        };
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| path.display().to_string());
        Ok(Self {
            path,
            name,
            platform: platform.to_owned(),
            serial,
            title_id,
            binary_digest,
        })
    }

    fn binary_digest(package: &dyn gumshoe::Package, path: &Path) -> Result<String, ScrapingError> {
        let binary = package
            .binaries(NormalizationOptions::decrypt_common())
            .next()
            .ok_or_else(|| ScrapingError::Binary {
                path: path.to_path_buf(),
                reason: "Gumshoe did not return a package binary".to_owned(),
            })?
            .map_err(|source| ScrapingError::Binary {
                path: path.to_path_buf(),
                reason: source.to_string(),
            })?;
        let digest =
            binary
                .digest::<blake3::Hasher>(None)
                .map_err(|source| ScrapingError::Binary {
                    path: path.to_path_buf(),
                    reason: source.to_string(),
                })?;
        let mut encoded = String::with_capacity(digest.len() * 2);
        for byte in digest {
            write!(&mut encoded, "{byte:02x}").expect("writing to a string cannot fail");
        }
        Ok(encoded)
    }
}

#[derive(Debug, Error)]
pub enum ScrapingError {
    #[error("cannot open package path {path}: {source}")]
    Open {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not identify {path}: {reason}")]
    Identify { path: PathBuf, reason: String },
    #[error("could not digest Gumshoe binary for {path}: {reason}")]
    Binary { path: PathBuf, reason: String },
    #[error(
        "{path} was identified as {platform}, which Snowglass does not map to a consolespec platform"
    )]
    UnsupportedPlatform { path: PathBuf, platform: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_fallback_digests_gumshoes_binary_instead_of_the_rom_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("headered.nes");
        let payload = [0x42; 32];
        let mut image = vec![0; 16];
        image[..4].copy_from_slice(b"NES\x1a");
        image.extend_from_slice(&payload);
        std::fs::write(&path, image).unwrap();

        let package = ScrapedPackage::from_path(path).unwrap();
        let expected = blake3::hash(&payload).to_hex();

        assert_eq!(package.platform, "NINTENDO_NES");
        assert_eq!(package.binary_digest.as_deref(), Some(expected.as_str()));
    }
}

fn consolespec_platform(platform: PlatformIdentifier) -> Option<&'static str> {
    use PlatformIdentifier::*;
    Some(match platform {
        Atari2600 => "ATARI_2600",
        Atari5200 => "ATARI_5200",
        Atari7800 => "ATARI_7800",
        AtariLynx => "ATARI_LYNX",
        AtariJaguar => "ATARI_JAGUAR",
        BandaiWonderSwan => "BANDAI_WS",
        BandaiWonderSwanColor => "BANDAI_WSC",
        BandaiSuFamiTurbo => "BANDAI_SUFAMI_TURBO",
        ColecoVision => "COLECO_CV",
        Intellivision => "MATTEL_INTELLIVISION",
        MagnavoxOdyssey2 => "MAGNAVOX_ODYSSEY2",
        GceVectrex => "GCE_VECTREX",
        MicrosoftXbox => "MICROSOFT_XBOX",
        MicrosoftXbox360 => "MICROSOFT_XBOX360",
        NecPcEngine => "NEC_TG16",
        NintendoGameBoy => "NINTENDO_GB",
        NintendoGameBoyColor => "NINTENDO_GBC",
        NintendoGameBoyAdvance => "NINTENDO_GBA",
        NintendoDS => "NINTENDO_DS",
        NintendoDSi => "NINTENDO_DSI",
        Nintendo3DS => "NINTENDO_3DS",
        NintendoNew3DS => "NINTENDO_NEW_3DS",
        NintendoNES => "NINTENDO_NES",
        NintendoFamicomDiskSystem => "NINTENDO_FDS",
        NintendoSuperNES => "NINTENDO_SNES",
        NintendoSatellaview => "NINTENDO_SATELLAVIEW",
        Nintendo64 => "NINTENDO_N64",
        Nintendo64DD => "NINTENDO_N64DD",
        NintendoVirtualBoy => "NINTENDO_VB",
        NintendoGameCube => "NINTENDO_GCN",
        NintendoWii => "NINTENDO_WII",
        NintendoWiiU => "NINTENDO_WIIU",
        NintendoSwitch => "NINTENDO_SWITCH",
        SegaGameGear => "SEGA_GG",
        SegaMasterSystem => "SEGA_SMS",
        SegaSG1000 => "SEGA_SG1000",
        SegaGenesis => "SEGA_GEN",
        SegaCD => "SEGA_CD",
        Sega32X => "SEGA_32X",
        Sega32XCD => "SEGA_32X_CD",
        SegaSaturn => "SEGA_SAT",
        SegaDreamcast => "SEGA_DC",
        SonyPlayStation => "SONY_PSX",
        SonyPlayStation2 => "SONY_PS2",
        SonyPlayStation3 => "SONY_PS3",
        SonyPlayStationPortable => "SONY_PSP",
        SonyPlayStationVita => "SONY_PSVITA",
        PhilipsCDi => "PHILIPS_CDI",
        SnkNeoGeoPocket => "SNK_NGP",
        SnkNeoGeoPocketColor => "SNK_NGPC",
        SnkNeoGeoCD => "SNK_NGCD",
        ThreeDO => "PANASONIC_3DO",
        WataraSupervision => "WATARA_SUPERVISION",
        _ => return None,
    })
}
