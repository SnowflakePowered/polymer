use crate::nand::VirtualNand;
use std::path::{Path, PathBuf};

pub mod artifacts;
mod component;
pub(crate) use component::ComponentStage;
mod directory;
mod installation;
mod progress;
mod writer;
pub(crate) use writer::StorageWriter;

pub use directory::DirectoryInstall;
pub use installation::{Installable, Installation};
pub(crate) use progress::Reporting;
pub use progress::{InstallContext, InstallEvent, InstallPhase, InstallPreview};

/// Discriminants are persisted in the artifact database, so they are fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
#[repr(u8)]
pub enum DepotType {
    /// System file.
    System = 0,
    /// A game or application.
    App = 1,
    /// DLC for a game or application.
    AppContent = 2,
    /// An update for a game or application.
    AppUpdate = 3,
}

impl From<DepotType> for u8 {
    fn from(depot_type: DepotType) -> u8 {
        depot_type as u8
    }
}

impl TryFrom<u8> for DepotType {
    /// The unrecognized discriminant.
    type Error = u8;

    fn try_from(id: u8) -> Result<Self, u8> {
        match id {
            0 => Ok(DepotType::System),
            1 => Ok(DepotType::App),
            2 => Ok(DepotType::AppContent),
            3 => Ok(DepotType::AppUpdate),
            other => Err(other),
        }
    }
}

#[allow(async_fn_in_trait)]
pub trait Depot: Sized + Send + 'static {
    /// Parse this depot type from a path. Invalid input must fail construction.
    fn open(path: &Path) -> Result<Self, crate::Error>;

    /// Keys embedded in this source, before an installation is constructed.
    fn embedded_keys(&self) -> Result<Vec<crate::keys::KeyRecord>, crate::Error> {
        Ok(Vec::new())
    }

    /// The title this depot installs, when it can be read without extraction.
    fn preview(&self) -> Option<InstallPreview> {
        None
    }

    /// Check whether the target NAND meets this depot's format and platform requirements.
    fn satisfies<N: VirtualNand + ?Sized>(&self, nand: &N) -> Result<(), crate::Error>;

    /// Validate the contents of the depot.
    ///
    /// Returns the names of the contents that don't validate.
    fn validate_contents(&self, _context: &InstallContext) -> Vec<String> {
        Vec::new()
    }

    fn depot_type(&self) -> DepotType;

    /// Prepare extraction into a clean, compatible NAND after the contents validate.
    async fn install<N: VirtualNand + ?Sized>(
        self,
        nand: &N,
        keys: &dyn crate::keys::KeyReader,
        context: &InstallContext,
    ) -> Result<impl Installable, crate::Error>;
}

#[derive(Clone)]
pub enum IconHint {
    /// No custom icon.
    Builtin(DepotType),
    /// The icon is a file in the installed artifact.
    File(PathBuf),
    /// A decoded icon stored in the Git metadata tree, never in the guest filesystem.
    Embedded(EmbeddedIcon),
}

/// An icon stored beside an artifact's metadata as `icon.png`, as `icon.arc`
/// for a Wii channel's animated icon layout, or as `icon.cbmd` for a 3DS
/// title's HOME Menu banner.
#[derive(Clone)]
pub enum EmbeddedIcon {
    Png(std::sync::Arc<[u8]>),
    /// A decompressed `icon.bin` archive that the shell renders and animates.
    WiiBanner(std::sync::Arc<[u8]>),
    /// A CBMD banner without its sound, rendered and animated by the shell.
    CtrBanner(std::sync::Arc<[u8]>),
}

impl EmbeddedIcon {
    pub(crate) const FILES: [&str; 3] = ["icon.png", "icon.arc", "icon.cbmd"];

    pub(crate) fn file_name(&self) -> &'static str {
        match self {
            Self::Png(_) => Self::FILES[0],
            Self::WiiBanner(_) => Self::FILES[1],
            Self::CtrBanner(_) => Self::FILES[2],
        }
    }

    pub(crate) fn from_file(name: &str, bytes: Vec<u8>) -> Option<Self> {
        match name {
            "icon.png" => Some(Self::Png(bytes.into())),
            "icon.arc" => Some(Self::WiiBanner(bytes.into())),
            "icon.cbmd" => Some(Self::CtrBanner(bytes.into())),
            _ => None,
        }
    }

    pub fn bytes(&self) -> &[u8] {
        match self {
            Self::Png(bytes) | Self::WiiBanner(bytes) | Self::CtrBanner(bytes) => bytes,
        }
    }

    pub fn data_url(&self) -> String {
        use base64::Engine as _;
        let media_type = match self {
            Self::Png(_) => "image/png",
            Self::WiiBanner(_) => polymer_banner::wii::WiiBanner::MEDIA_TYPE,
            Self::CtrBanner(_) => polymer_banner::ctr::CtrBanner::MEDIA_TYPE,
        };
        let encoded = base64::engine::general_purpose::STANDARD.encode(self.bytes());
        format!("data:{media_type};base64,{encoded}")
    }
}

impl IconHint {
    pub(crate) fn image(image: image::RgbaImage) -> Result<Self, crate::Error> {
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut png, image::ImageFormat::Png)
            .map_err(|error| crate::Error::malformed("icon", error.to_string()))?;
        Ok(Self::Embedded(EmbeddedIcon::Png(png.into_inner().into())))
    }

    pub(crate) fn read(
        storage: &dyn crate::Storage,
        path: &str,
        kind: DepotType,
    ) -> Result<Self, crate::Error> {
        if !storage.exists(path) {
            return Ok(Self::Builtin(kind));
        }
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut storage.open_read(path)?, &mut bytes)
            .map_err(|error| crate::Error::io(Path::new(path), error))?;
        let decoded = if path.ends_with(".tga") {
            image::load_from_memory_with_format(&bytes, image::ImageFormat::Tga)
        } else {
            image::load_from_memory(&bytes)
        };
        match decoded {
            Ok(image) => Self::image(image.into_rgba8()),
            Err(_) => Ok(Self::Builtin(kind)),
        }
    }
}

#[derive(Clone)]
pub struct DepotFile {
    path: PathBuf,
    blake3: [u8; 32],
    byte_length: u64,
}

impl DepotFile {
    /// Merge retained ownership with this installation; new paths win.
    pub(crate) fn merge(retained: Vec<Self>, installed: Vec<Self>) -> Vec<Self> {
        let mut files = std::collections::BTreeMap::new();
        for file in retained.into_iter().chain(installed) {
            files.insert(file.path.clone(), file);
        }
        files.into_values().collect()
    }
    pub fn new(path: PathBuf, blake3: [u8; 32], byte_length: u64) -> Self {
        Self {
            path,
            blake3,
            byte_length,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn blake3(&self) -> &[u8; 32] {
        &self.blake3
    }

    pub fn byte_length(&self) -> u64 {
        self.byte_length
    }
}
