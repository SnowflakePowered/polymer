use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use zerocopy::byteorder::big_endian::{U16, U32};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use super::tmd::{Content, Tmd, align};
use crate::Error;
use crate::impls::wii::nand::WiiNand;
use crate::impls::wii::ticket::Ticket;
use crate::installable::{Depot, DepotType};
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

pub struct WiiWadDepot {
    pub(super) path: PathBuf,
    pub(super) ticket: Ticket,
    pub(super) tmd: Tmd,
    tmd_offset: u64,
    tmd_size: u64,
}

impl WiiWadDepot {
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
        Self::read_section(&mut file, self.tmd_offset, self.tmd_size, &self.path)
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
        })
    }

    fn satisfies<N: VirtualNand + ?Sized>(&self, nand: &N) -> Result<(), Error> {
        if nand.is_dirty()? {
            return Err(Error::HistoryConflict(
                "This NAND has uncommitted changes. Recover it in Console storage before installing.".into(),
            ));
        }
        if !(nand.platform() == WiiNand::PLATFORM && WiiNand::has_structure(&*nand.root())) {
            return Err(Error::Unsupported(
                "Depot requires a compatible NAND with its storage layout initialized".into(),
            ));
        }
        Ok(())
    }
    fn validate_contents(&self) -> Vec<String> {
        let Ok(key) = self.select_title_key() else {
            return vec!["ticket".to_owned()];
        };
        self.tmd
            .contents
            .iter()
            .filter_map(|content| {
                self.decrypt_content(content, &key)
                    .err()
                    .map(|_| format!("{:08x}.app", content.id))
            })
            .collect()
    }

    fn depot_type(&self) -> DepotType {
        if (self.title_id() >> 32) as u32 == 1 {
            DepotType::System
        } else {
            DepotType::App
        }
    }

    fn install<'a, N: VirtualNand + ?Sized>(
        self,
        nand: &'a N,
    ) -> crate::installable::Installation<'a>
    where
        Self: 'a,
    {
        let source = self;

        crate::installable::Installation::prepare(async move {
            source.validate(nand, &source.path)?;
            super::install::WadInstall::new(source, nand.root()).await
        })
    }
}
