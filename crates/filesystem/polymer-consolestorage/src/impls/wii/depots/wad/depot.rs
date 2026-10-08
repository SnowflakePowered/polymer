use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use zerocopy::byteorder::big_endian::{U16, U32};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use super::tmd::{Content, Tmd, align};
use crate::Error;
use crate::impls::wii::nand::WiiNand;
use crate::impls::wii::ticket::Ticket;
use crate::installable::{
    Depot, DepotType, EmbeddedIcon, InstallContext, InstallPreview, Installable,
};
use crate::keys::{KeyIdentity, KeyRecord};
use crate::nand::VirtualNand;

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct WadHeader {
    header_size: U32,
    kind: U16,
    version: U16,
    certificate_size: U32,
    reserved: U32,
    ticket_size: U32,
    tmd_size: U32,
    data_size: U32,
    footer_size: U32,
}

const _: () = assert!(size_of::<WadHeader>() == 0x20);

/// The channel name and animated icon layout from a WAD's opening banner.
#[derive(Clone, Default)]
pub(super) struct Channel {
    pub(super) name: Option<String>,
    pub(super) icon: Option<EmbeddedIcon>,
}

#[derive(Clone)]
pub struct WiiWadDepot {
    pub(super) path: PathBuf,
    pub(super) ticket: Ticket,
    pub(super) tmd: Tmd,
    tmd_offset: u64,
    tmd_size: u64,
    tmd_digest: blake3::Hash,
    channel: std::sync::OnceLock<Channel>,
}

impl WiiWadDepot {
    /// The channel's name and icon archive from its opening banner (content 0),
    /// decrypted once per depot.
    pub(super) fn channel(&self) -> &Channel {
        self.channel.get_or_init(|| {
            let opening = self.select_title_key().ok().and_then(|key| {
                let content = self
                    .tmd
                    .contents
                    .iter()
                    .find(|content| content.index == 0)?;
                self.decrypt_content(content, &key).ok()
            });
            let Some(opening) = opening else {
                return Channel::default();
            };
            Channel {
                name: super::metadata::InstalledTitle::channel_name(&opening),
                icon: polymer_banner::wii::WiiBanner::icon_archive(&opening)
                    .map(|archive| EmbeddedIcon::WiiBanner(archive.into())),
            }
        })
    }

    pub fn title_id(&self) -> u64 {
        self.tmd.title_id
    }

    pub fn title_version(&self) -> u16 {
        self.tmd.title_version
    }

    pub(super) fn read_encrypted(&self, content: &Content) -> Result<Vec<u8>, Error> {
        let mut file = File::open(&self.path).map_err(|source| Error::io(&self.path, source))?;
        Self::read_section(
            &mut file,
            content.wad_offset,
            align(content.size, 0x40)?,
            &self.path,
        )
    }

    pub(super) fn read_tmd(&self) -> Result<Vec<u8>, Error> {
        let mut file = File::open(&self.path).map_err(|source| Error::io(&self.path, source))?;
        let bytes = Self::read_section(&mut file, self.tmd_offset, self.tmd_size, &self.path)?;
        if blake3::hash(&bytes) != self.tmd_digest {
            return Err(Error::IntegrityMatchFailed {
                failed: "WAD title metadata".into(),
            });
        }
        Ok(bytes)
    }

    pub(super) fn signature_is_zero(&self) -> bool {
        self.ticket.fakesigned
    }

    fn section_end(offset: u64, size: u64) -> Result<u64, Error> {
        align(
            offset
                .checked_add(size)
                .ok_or_else(|| Error::malformed("WAD", "section range overflows"))?,
            0x40,
        )
    }

    fn read_section(
        file: &mut File,
        offset: u64,
        size: u64,
        path: &Path,
    ) -> Result<Vec<u8>, Error> {
        let size = usize::try_from(size)
            .map_err(|_| Error::malformed("WAD", "section is too large for this host"))?;
        file.seek(SeekFrom::Start(offset))
            .and_then(|_| {
                let mut bytes = vec![0; size];
                file.read_exact(&mut bytes)?;
                Ok(bytes)
            })
            .map_err(|source| Error::io(path, source))
    }
}

impl Depot for WiiWadDepot {
    fn embedded_keys(&self) -> Result<Vec<KeyRecord>, Error> {
        let key = self.select_title_key()?;
        let title_id = format!("{:016x}", self.title_id());
        let record = KeyRecord {
            identity: KeyIdentity {
                namespace: "content".into(),
                id: title_id.clone(),
            },
            title_id: Some(title_id),
            bytes: key.to_vec(),
        };

        Ok(vec![record])
    }

    fn open(path: &Path) -> Result<Self, Error> {
        let mut file = File::open(path).map_err(|source| Error::io(path, source))?;
        let file_size = file
            .metadata()
            .map_err(|source| Error::io(path, source))?
            .len();
        let mut header_bytes = [0; size_of::<WadHeader>()];
        file.read_exact(&mut header_bytes)
            .map_err(|source| Error::io(path, source))?;
        let header = WadHeader::ref_from_bytes(&header_bytes)
            .map_err(|_| Error::malformed("WAD", "invalid header layout"))?;
        if header.header_size.get() < size_of::<WadHeader>() as u32 {
            return Err(Error::malformed("WAD", "header is smaller than 0x20 bytes"));
        }
        let certificate_offset = align(u64::from(header.header_size.get()), 0x40)?;
        let ticket_offset =
            Self::section_end(certificate_offset, u64::from(header.certificate_size.get()))?;
        let tmd_offset = Self::section_end(ticket_offset, u64::from(header.ticket_size.get()))?;
        let data_offset = Self::section_end(tmd_offset, u64::from(header.tmd_size.get()))?;
        let footer_offset = Self::section_end(data_offset, u64::from(header.data_size.get()))?;
        let end = footer_offset
            .checked_add(u64::from(header.footer_size.get()))
            .ok_or_else(|| Error::malformed("WAD", "footer range overflows"))?;
        if end > file_size {
            return Err(Error::malformed("WAD", "declared sections exceed the file"));
        }
        let ticket_bytes = Self::read_section(
            &mut file,
            ticket_offset,
            u64::from(header.ticket_size.get()),
            path,
        )?;
        let tmd_size = u64::from(header.tmd_size.get());
        let tmd_bytes = Self::read_section(&mut file, tmd_offset, tmd_size, path)?;
        let ticket = Ticket::parse(ticket_bytes)?;
        let tmd = Tmd::parse(&tmd_bytes, data_offset, u64::from(header.data_size.get()))?;
        if ticket.title_id != tmd.title_id {
            return Err(Error::malformed("WAD", "ticket and TMD title IDs differ"));
        }
        Ok(Self {
            path: path.to_path_buf(),
            ticket,
            tmd,
            tmd_offset,
            tmd_size,
            tmd_digest: blake3::hash(&tmd_bytes),
            channel: std::sync::OnceLock::new(),
        })
    }

    fn satisfies<N: VirtualNand + ?Sized>(&self, nand: &N) -> Result<(), Error> {
        if !(nand.platform() == WiiNand::PLATFORM && WiiNand::has_structure(&*nand.root())) {
            return Err(Error::IncompatibleNand);
        }
        Ok(())
    }

    fn validate_contents(&self, context: &InstallContext) -> Vec<String> {
        let Ok(key) = self.select_title_key() else {
            return vec!["ticket".to_owned()];
        };
        context.phase(
            crate::installable::InstallPhase::Verifying,
            self.tmd.contents.iter().map(|content| content.size).sum(),
        );
        self.tmd
            .contents
            .iter()
            .filter_map(|content| {
                let verified = self.decrypt_content(content, &key);
                context.advance(content.size);
                verified.err().map(|_| format!("{:08x}.app", content.id))
            })
            .collect()
    }

    fn preview(&self) -> Option<InstallPreview> {
        let channel = self.channel();
        let id = format!("{:016x}", self.title_id());
        Some(InstallPreview {
            label: channel.name.clone().unwrap_or_else(|| id.clone()),
            id,
            depot_type: self.depot_type(),
            icon: channel.icon.as_ref().map(EmbeddedIcon::data_url),
        })
    }

    fn depot_type(&self) -> DepotType {
        crate::impls::wii::composer::WiiComposer::title_kind(self.title_id())
    }

    async fn install<N: VirtualNand + ?Sized>(
        self,
        _nand: &N,
        _keys: &dyn crate::keys::KeyReader,
        context: &InstallContext,
    ) -> Result<impl Installable, Error> {
        let stage = crate::installable::ComponentStage::new()?;
        let component = crate::impls::wii::composer::WiiComposer::component(self.title_id());
        let contribution = crate::impls::wii::composer::WiiContribution {
            title_id: self.title_id(),
            required_ios: self.tmd.system_version,
            shared: self
                .tmd
                .contents
                .iter()
                .filter(|content| content.is_shared())
                .map(|content| {
                    (
                        format!("{}/{:08x}.app", component.root, content.id),
                        content.sha1,
                    )
                })
                .collect(),
        };
        let metadata = serde_json::to_value(contribution)
            .map_err(|error| Error::malformed("Wii component", error.to_string()))?;
        let inner = super::install::WadInstall::new(self, stage.storage.clone(), context)?;
        Ok(stage.install(inner, component, metadata))
    }
}
