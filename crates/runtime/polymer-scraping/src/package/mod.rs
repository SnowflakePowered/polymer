mod files;
mod identity;

use gumshoe::{
    GeneralIdentifier, IdentifyPackage, PlatformIdentifier,
    banner::{BannerKind, DecodedBanner, Sample},
    binary::NormalizationOptions,
};
use image::{DynamicImage, ImageFormat, codecs::gif::GifEncoder};
use polymer_library::{
    GameCatalogError, Launchable, MetadataInput, MetadataInputData, MetadataValue,
};
use shiratsu_naming::naming::{
    TokenizedName,
    goodtools::{GoodToolsName, GoodToolsToken},
    nointro::{NoIntroName, NoIntroToken},
    tosec::{TOSECName, TOSECToken},
};
use std::{
    collections::BTreeSet,
    io::Cursor,
    path::{Path, PathBuf},
};

/// A Gumshoe-identified game package shared by the host and frontend.
///
/// Metadata and extracted media stay host-private while the package is serialized
/// into the frontend realm. A package produced by [`Package::identify`] can later
/// be imported without extracting metadata a second time. Binary hashing is
/// deferred until a digest request, library import, or launch needs it.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Package {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle: Option<crate::LaunchBundle>,
    #[serde(
        default,
        rename = "__manifest",
        skip_serializing_if = "Option::is_none"
    )]
    manifest: Option<polymer_library::PackageManifest>,
    #[serde(default, rename = "__digestPath")]
    pub path: PathBuf,
    pub filename: String,
    pub platform: String,
    pub serial: Option<String>,
    pub title_id: Option<String>,
    pub binary_digest: String,
    #[serde(skip)]
    pub installable: bool,
    #[serde(skip)]
    import: Option<PackageImport>,
}

#[derive(Clone, Debug)]
struct PackageImport {
    parent_binary_id: Option<uuid::Uuid>,
    gumshoe: Vec<MetadataInput>,
    filename: Vec<MetadataInput>,
}

impl Package {
    /// Builds a runtime package from registered metadata without reading the game.
    /// Import-time identification and media extraction are deliberately omitted.
    pub fn from_launchable(launchable: Launchable) -> Result<Self, GameCatalogError> {
        let digest = launchable.binary_blake3.as_ref().try_into().map_err(|_| {
            GameCatalogError::InvalidData("invalid library binary digest".to_owned())
        })?;
        let filename = launchable
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| launchable.path.display().to_string());
        Ok(Self {
            bundle: None,
            manifest: launchable.manifest,
            path: launchable.path,
            filename,
            platform: launchable.platform,
            serial: launchable.serial,
            title_id: launchable.title_id,
            binary_digest: blake3::Hash::from_bytes(digest).to_hex().to_string(),
            installable: false,
            import: None,
        })
    }

    /// Prepare launch data, computing identity only for an unregistered package.
    pub fn prepare_launch(&mut self) -> Result<(), crate::ScrapingError> {
        self.ensure_binary_identity()?;
        if let Some(manifest) = &mut self.manifest {
            if manifest.template.is_none() {
                crate::ingest::CuePreparation::finalize(manifest)?;
            }
            self.bundle = Some(crate::ingest::CuePreparation::bundle(manifest)?);
        }
        Ok(())
    }

    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, crate::ScrapingError> {
        Self::identify(path, None, None)
    }

    pub fn identify(
        path: impl AsRef<Path>,
        platform_hint: Option<&str>,
        parent_binary_id: Option<uuid::Uuid>,
    ) -> Result<Self, crate::ScrapingError> {
        let requested = path.as_ref();
        let path = requested
            .canonicalize()
            .map_err(|source| crate::ScrapingError::Open {
                path: requested.to_owned(),
                source,
            })?;
        let entries = crate::Ingestor::entrypoints(&path)?;
        // A single package API retains Gumshoe's rejection of ambiguous directories.
        let path = if entries.len() == 1 {
            entries[0].clone()
        } else {
            path
        };
        let package = GeneralIdentifier.identify_path(&path).map_err(|source| {
            crate::ScrapingError::Detection {
                path: path.clone(),
                source,
            }
        })?;
        let manifest = crate::ingest::CuePreparation::capture(&path, package.as_ref())?;
        let path = manifest
            .as_ref()
            .map(|value| value.descriptor.path.clone())
            .unwrap_or(path);
        let inferred = Self::platform_identifier(package.platform());
        let hinted = platform_hint
            .map(|platform| {
                consolespec::MachineSpec::try_from(platform)
                    .map(|machine| machine.id())
                    .map_err(|_| crate::ScrapingError::InvalidPlatformHint(platform.to_owned()))
            })
            .transpose()?;
        if let (Some(inferred), Some(hinted)) = (inferred, hinted)
            && inferred != hinted
        {
            return Err(crate::ScrapingError::PlatformMismatch {
                inferred: inferred.to_owned(),
                supplied: hinted.to_owned(),
            });
        }
        let platform =
            inferred
                .or(hinted)
                .ok_or_else(|| crate::ScrapingError::UnsupportedPlatform {
                    path: path.clone(),
                    platform: format!("{:?}", package.platform()),
                })?;
        let serial = package.serial().map(str::to_owned);
        let title_id = package.titleid();
        let gumshoe = Self::gumshoe_metadata(package.as_ref(), platform);
        let filename = Self::filename_metadata(&path);
        if let Some(manifest) = &manifest {
            crate::ingest::CuePreparation::validate_files(manifest)?;
        }
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| path.display().to_string());
        Ok(Self {
            bundle: None,
            manifest,
            path,
            filename: name,
            platform: platform.to_owned(),
            serial,
            title_id,
            binary_digest: String::new(),
            installable: package.installable(),
            import: Some(PackageImport {
                parent_binary_id,
                gumshoe,
                filename,
            }),
        })
    }

    fn scalar(key: &str, source: &str, value: impl Into<String>) -> MetadataInput {
        MetadataInput {
            key: key.to_owned(),
            source: source.to_owned(),
            data: MetadataInputData::Scalar {
                value: MetadataValue::String(value.into()),
            },
        }
    }

    fn gumshoe_metadata(package: &dyn gumshoe::Package, platform: &str) -> Vec<MetadataInput> {
        let mut result = vec![Self::scalar("package.platform", "Gumshoe", platform)];
        if let Some(serial) = package.serial() {
            result.push(Self::scalar("package.serial", "Gumshoe", serial));
        }
        if let Some(title_id) = package.titleid() {
            result.push(Self::scalar("package.titleid", "Gumshoe", title_id));
        }
        let mut names = BTreeSet::new();
        if let Some(name) = package.internal_name(None, NormalizationOptions::decrypt_common()) {
            names.insert((None, name.to_owned()));
        }
        for language in language::Language::all() {
            if let Some(name) =
                package.internal_name(Some(language), NormalizationOptions::decrypt_common())
            {
                names.insert((Some(language.tag()), name.to_owned()));
            }
        }
        for (language, name) in names {
            result.push(Self::scalar(
                &language.map_or_else(
                    || "package.internalname".to_owned(),
                    |tag| format!("package.internalname.{tag}"),
                ),
                "Gumshoe",
                name,
            ));
        }
        for banner in package
            .banners(NormalizationOptions::decrypt_common())
            .flatten()
        {
            let kind = banner.kind();
            if let Ok(decoded) = banner.render()
                && let Some((key, media_type, bytes)) = Self::banner_bytes(kind, decoded)
            {
                result.push(MetadataInput {
                    key,
                    source: "Gumshoe".to_owned(),
                    data: MetadataInputData::Inline {
                        media_type,
                        bytes: bytes.into_boxed_slice(),
                        source_url: None,
                    },
                });
            }
        }
        result
    }

    fn banner_bytes(kind: BannerKind, decoded: DecodedBanner) -> Option<(String, String, Vec<u8>)> {
        let key = match kind {
            BannerKind::Icon => "banner.icon",
            BannerKind::Banner | BannerKind::Video => "banner.banner",
            BannerKind::Background => "banner.background",
            BannerKind::Title => "banner.logo",
            BannerKind::Audio => "banner.audio",
        }
        .to_owned();
        match decoded {
            DecodedBanner::Image(image) => {
                let mut bytes = Cursor::new(Vec::new());
                DynamicImage::ImageRgba8(image)
                    .write_to(&mut bytes, ImageFormat::Png)
                    .ok()?;
                Some((key, "image/png".to_owned(), bytes.into_inner()))
            }
            DecodedBanner::Animation(frames) => {
                let frames = frames.collect::<Result<Vec<_>, _>>().ok()?;
                let mut bytes = Vec::new();
                GifEncoder::new(&mut bytes).encode_frames(frames).ok()?;
                Some((key, "image/gif".to_owned(), bytes))
            }
            DecodedBanner::Vector(value) => {
                Some((key, "image/svg+xml".to_owned(), value.into_bytes()))
            }
            DecodedBanner::H264BitStream(value) => Some((key, "video/h264".to_owned(), value)),
            DecodedBanner::Audio(samples) => {
                let mut bytes = Vec::new();
                for sample in samples.flatten() {
                    let value = match sample {
                        Sample::U8(v) => f32::from(v) / 255.0,
                        Sample::I8(v) => f32::from(v) / 128.0,
                        Sample::I16(v) => f32::from(v) / 32768.0,
                        Sample::I24(v) => v as f32 / 8_388_608.0,
                        Sample::I32(v) => v as f32 / 2_147_483_648.0,
                        Sample::F32(v) => v,
                        Sample::F64(v) => v as f32,
                    };
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
                Some(("banner.audio".to_owned(), "audio/x-raw".to_owned(), bytes))
            }
            DecodedBanner::Scene(scene) => {
                let mut bytes = Vec::new();
                scene.to_writer(&mut bytes).ok()?;
                Some((
                    "banner.3d".to_owned(),
                    "model/gltf-binary".to_owned(),
                    bytes,
                ))
            }
            _ => None,
        }
    }

    fn filename_metadata(path: &Path) -> Vec<MetadataInput> {
        let raw = path
            .file_name()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let stem = path
            .file_stem()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| raw.clone());
        let mut result = vec![Self::scalar("filename.raw", "Filename", raw)];
        if let Ok(name) = NoIntroName::try_parse(&stem) {
            if let Some(title) = name.title() {
                result.push(Self::scalar(
                    "filename.nointro.title",
                    "Shiratsu No-Intro",
                    title,
                ));
            }
            for token in name.iter() {
                Self::push_nointro(&mut result, token);
            }
        }
        if let Ok(name) = TOSECName::try_parse(&stem) {
            if let Some(title) = name.title() {
                result.push(Self::scalar(
                    "filename.tosec.title",
                    "Shiratsu TOSEC",
                    title,
                ));
            }
            for token in name.iter() {
                Self::push_tosec(&mut result, token);
            }
        }
        if let Ok(name) = GoodToolsName::try_parse(&stem) {
            if let Some(title) = name.title() {
                result.push(Self::scalar(
                    "filename.goodtools.title",
                    "Shiratsu GoodTools",
                    title,
                ));
            }
            for token in name.iter() {
                Self::push_goodtools(&mut result, token);
            }
        }
        result
    }

    fn push_nointro(result: &mut Vec<MetadataInput>, token: &NoIntroToken<'_>) {
        let (field, value) = match token {
            NoIntroToken::Title(_) => return,
            NoIntroToken::Region { region_strings, .. } => ("region", region_strings.join(",")),
            NoIntroToken::Version(value) => ("version", format!("{value:?}")),
            NoIntroToken::Release { status, number } => (
                "release",
                format!(
                    "{status}{}",
                    number.map(|v| format!(" {v}")).unwrap_or_default()
                ),
            ),
            NoIntroToken::Media { media_type, number } => {
                ("media", format!("{media_type} {number}"))
            }
            NoIntroToken::Scene { number, prefix } => {
                ("scene", format!("{}{number}", prefix.unwrap_or_default()))
            }
            NoIntroToken::Languages(value) => ("language", format!("{value:?}")),
            NoIntroToken::Flag { flag, .. } => ("flag", (*flag).to_owned()),
        };
        result.push(Self::scalar(
            &format!("filename.nointro.{field}"),
            "Shiratsu No-Intro",
            value,
        ));
    }

    fn push_tosec(result: &mut Vec<MetadataInput>, token: &TOSECToken<'_>) {
        let (field, values) = match token {
            TOSECToken::Title(_) => return,
            TOSECToken::Version {
                version_type,
                major,
                minor,
            } => (
                "version",
                vec![format!(
                    "{version_type} {major}{}",
                    minor.map(|value| format!(".{value}")).unwrap_or_default()
                )],
            ),
            TOSECToken::Demo(value) => ("demo", vec![value.unwrap_or("demo").to_owned()]),
            TOSECToken::Date { year, month, day } => (
                "date",
                vec![format!(
                    "{year}{}{}",
                    month.map(|value| format!("-{value}")).unwrap_or_default(),
                    day.map(|value| format!("-{value}")).unwrap_or_default()
                )],
            ),
            TOSECToken::Publisher(values) => (
                "publisher",
                vec![
                    values
                        .as_ref()
                        .map(|values| values.join(" - "))
                        .unwrap_or_else(|| "-".to_owned()),
                ],
            ),
            TOSECToken::System(value) => ("system", vec![(*value).to_owned()]),
            TOSECToken::Video(value) => ("video", vec![(*value).to_owned()]),
            TOSECToken::Region { region_strings, .. } => ("region", vec![region_strings.join(",")]),
            TOSECToken::Languages(value) => ("language", vec![format!("{value:?}")]),
            TOSECToken::Copyright(value) => ("copyright", vec![(*value).to_owned()]),
            TOSECToken::Development(value) => ("development", vec![(*value).to_owned()]),
            TOSECToken::DumpInfo { code, number, info } => (
                "dumpinfo",
                vec![format!(
                    "{code}{}{}",
                    number.unwrap_or_default(),
                    info.map(|value| format!(" {value}")).unwrap_or_default()
                )],
            ),
            TOSECToken::Media(values) => (
                "media",
                values
                    .iter()
                    .map(|value| {
                        format!(
                            "{} {}{}",
                            value.media_type,
                            value.number,
                            value
                                .total
                                .map(|total| format!(" of {total}"))
                                .unwrap_or_default()
                        )
                    })
                    .collect(),
            ),
            TOSECToken::Flag { flag, .. } => ("flag", vec![(*flag).to_owned()]),
            TOSECToken::Warning(value) => ("warning", vec![format!("{value:?}")]),
        };
        for value in values {
            result.push(Self::scalar(
                &format!("filename.tosec.{field}"),
                "Shiratsu TOSEC",
                value,
            ));
        }
    }

    fn push_goodtools(result: &mut Vec<MetadataInput>, token: &GoodToolsToken<'_>) {
        let (field, value) = match token {
            GoodToolsToken::Title(_) => return,
            GoodToolsToken::Region { region_strings, .. } => ("region", region_strings.join(",")),
            GoodToolsToken::Year(value) => ("year", (*value).to_owned()),
            GoodToolsToken::MultiLanguage(value) => ("multilanguage", (*value).to_owned()),
            GoodToolsToken::Version { .. } => ("version", format!("{token:?}")),
            GoodToolsToken::Volume(value) => ("volume", (*value).to_owned()),
            GoodToolsToken::NInOne { .. } => ("ninone", format!("{token:?}")),
            GoodToolsToken::DumpCode { .. } => ("dumpcode", format!("{token:?}")),
            GoodToolsToken::GameHack(value) => ("gamehack", value.unwrap_or("Hack").to_owned()),
            GoodToolsToken::Media { .. } => ("media", format!("{token:?}")),
            GoodToolsToken::Translation { .. } => ("translation", format!("{token:?}")),
            GoodToolsToken::Flag { flag, .. } => ("flag", (*flag).to_owned()),
        };
        result.push(Self::scalar(
            &format!("filename.goodtools.{field}"),
            "Shiratsu GoodTools",
            value,
        ));
    }
}

impl Package {
    pub fn platform_identifier(platform: PlatformIdentifier) -> Option<&'static str> {
        use PlatformIdentifier::*;
        Some(match platform {
            Atari2600 => "ATARI_2600",
            Atari5200 => "ATARI_5200",
            Atari7800 => "ATARI_7800",
            AtariLynx => "ATARI_LYNX",
            AtariJaguar => "ATARI_JAGUAR",
            BandaiWonderSwan => "BANDAI_WS",
            BandaiWonderSwanColor => "BANDAI_WSC",
            BandaiSuFamiTurbo => "NINTENDO_SFT",
            ColecoVision => "COLECO_CV",
            Intellivision => "MATTEL_INT",
            MagnavoxOdyssey2 => "MAGNAVOX_O2",
            GceVectrex => "GCE_VECTREX",
            MicrosoftXbox => "MICROSOFT_XBOX",
            MicrosoftXbox360 => "MICROSOFT_X360",
            NecPcEngine => "NEC_TG16",
            NintendoGameBoy => "NINTENDO_GB",
            NintendoGameBoyColor => "NINTENDO_GBC",
            NintendoGameBoyAdvance => "NINTENDO_GBA",
            NintendoDS => "NINTENDO_NDS",
            NintendoDSi => "NINTENDO_DSI",
            Nintendo3DS => "NINTENDO_3DS",
            NintendoNew3DS => "NINTENDO_N3DS",
            NintendoNES => "NINTENDO_NES",
            NintendoFamicomDiskSystem => "NINTENDO_FDS",
            NintendoSuperNES => "NINTENDO_SNES",
            NintendoSatellaview => "NINTENDO_BS",
            Nintendo64 => "NINTENDO_N64",
            Nintendo64DD => "NINTENDO_N64DD",
            NintendoVirtualBoy => "NINTENDO_VB",
            NintendoGameCube => "NINTENDO_GCN",
            NintendoWii => "NINTENDO_WII",
            NintendoWiiU => "NINTENDO_WIIU",
            NintendoSwitch => "NINTENDO_NSW",
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
            SonyPlayStationVita => "SONY_PSV",
            PhilipsCDi => "PHILIPS_CDI",
            SnkNeoGeoPocket => "SNK_NGP",
            SnkNeoGeoPocketColor => "SNK_NGPC",
            SnkNeoGeoCD => "SNK_NGCD",
            ThreeDO => "PANASONIC_3DO",
            WataraSupervision => "WATARA_SV",
            _ => return None,
        })
    }
}
