use gumshoe::{GeneralIdentifier, IdentifyPackage, PlatformIdentifier};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Immutable package identity produced by Gumshoe and exposed to the UI realm.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrapedPackage {
    pub path: PathBuf,
    pub name: String,
    pub platform: String,
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
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| path.display().to_string());
        Ok(Self {
            path,
            name,
            platform: platform.to_owned(),
        })
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
    #[error(
        "{path} was identified as {platform}, which Snowglass does not map to a consolespec platform"
    )]
    UnsupportedPlatform { path: PathBuf, platform: String },
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
