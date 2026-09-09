use std::collections::HashSet;
use std::fs::File;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use futures::TryFutureExt;
use futures::channel::oneshot;
use futures::stream::{self, StreamExt};
use zerocopy::byteorder::big_endian::{U16, U32};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use crate::impls::wii::index::{SharedContentMap, UidMap};
use super::ticket::Ticket;
use super::tmd::{Content, Tmd, align};
use crate::Error;
use crate::impls::wii::nand::WiiNand;
use crate::installable::artifacts::Artifact;
use crate::installable::{Depot, DepotFile, DepotType, IconHint};
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
    pub fn open(path: &Path) -> Result<Self, Error> {
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
    fn satisfies<N: VirtualNand>(&self, nand: &N) -> bool {
        nand.platform() == WiiNand::PLATFORM && WiiNand::has_structure(&*nand.root())
    }

    fn validate_contents(&self) -> Vec<String> {
        let Ok(key) = self.select_title_key() else {
            return vec!["ticket".to_owned()];
        };
        self.tmd
            .contents
            .iter()
            .filter_map(|content| {
                self.decrypt_content(content, &key.key)
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

    fn install<'a, N: VirtualNand>(
        &'a self,
        nand: &'a N,
    ) -> (
        Box<dyn Future<Output = Result<Artifact, Error>>>,
        Box<dyn futures::Stream<Item = Result<String, Error>> + 'a>,
    ) {
        let (sender, receiver) = oneshot::channel();
        let operation = stream::once(async move {
            let result = async {
                if !self.satisfies(nand) {
                    return Err(Error::Unsupported(format!(
                        "a Wii WAD cannot be installed to {}",
                        nand.platform()
                    )));
                }
                let selected = self.select_title_key()?;

                let storage = nand.root();
                let artifact_id = format!("{:016x}", self.title_id());
                let old = storage.artifacts().artifact(&artifact_id).await?;
                let mut shared = SharedContentMap::load(&*storage)?;
                let mut shared_changed = false;
                let mut manifest = Vec::new();
                let mut installed = Vec::new();
                let content_root =
                    format!("{}/content", crate::impls::wii::title_path(self.title_id()));

                for content in &self.tmd.contents {
                    let bytes = self.decrypt_content(content, &selected.key)?;
                    let path = if content.is_shared() {
                        let id = match shared.find(&content.sha1) {
                            Some(entry) => entry.id.clone(),
                            None => {
                                let id = shared.next_id()?;
                                let path = format!("shared1/{id}.app");
                                let copied = storage
                                    .install_file(Box::new(Cursor::new(bytes.clone())), &path)
                                    .await?;
                                manifest.push(DepotFile::new(
                                    PathBuf::from(&path),
                                    copied.blake3,
                                    copied.byte_length,
                                ));
                                installed.push(path);
                                shared.add(id.clone(), content.sha1);
                                shared_changed = true;
                                id
                            }
                        };
                        format!("shared1/{id}.app")
                    } else {
                        format!("{content_root}/{:08x}.app", content.id)
                    };

                    if !manifest
                        .iter()
                        .any(|file| file.path().to_string_lossy() == path)
                    {
                        let copied = if content.is_shared()
                            && storage.exists(&path)
                            && SharedContentMap::file_matches(&*storage, &path, &content.sha1)
                        {
                            crate::InstallResult {
                                blake3: *blake3::hash(&bytes).as_bytes(),
                                byte_length: bytes.len() as u64,
                            }
                        } else {
                            let copied = storage
                                .install_file(Box::new(Cursor::new(bytes)), &path)
                                .await?;
                            installed.push(path.clone());
                            copied
                        };
                        manifest.push(DepotFile::new(
                            PathBuf::from(&path),
                            copied.blake3,
                            copied.byte_length,
                        ));
                    }
                }

                if shared_changed {
                    shared.write(&*storage).await?;
                    installed.push("shared1/content.map".to_owned());
                }

                let ticket_path = format!(
                    "ticket/{:08x}/{:08x}.tik",
                    self.title_id() >> 32,
                    self.title_id() as u32
                );
                let ticket = self.ticket.bytes_with_key_index(selected.index);
                let copied = storage
                    .install_file(Box::new(Cursor::new(ticket)), &ticket_path)
                    .await?;
                manifest.push(DepotFile::new(
                    PathBuf::from(&ticket_path),
                    copied.blake3,
                    copied.byte_length,
                ));
                installed.push(ticket_path);

                UidMap::load(&*storage)?
                    .insert(&*storage, self.title_id())
                    .await?;

                let tmd_path = format!("{content_root}/title.tmd");
                let copied = storage
                    .install_file(Box::new(Cursor::new(self.read_tmd()?)), &tmd_path)
                    .await?;
                manifest.push(DepotFile::new(
                    PathBuf::from(&tmd_path),
                    copied.blake3,
                    copied.byte_length,
                ));
                installed.push(tmd_path);

                if let Some(old) = old {
                    let current = manifest
                        .iter()
                        .map(|file| file.path().to_string_lossy().into_owned())
                        .collect::<HashSet<_>>();
                    for file in old.manifest() {
                        let path = file.path().to_string_lossy();
                        if path.starts_with(&content_root) && !current.contains(path.as_ref()) {
                            storage.remove(&path)?;
                        }
                    }
                }

                let artifact = Artifact::new(
                    artifact_id,
                    self.depot_type(),
                    manifest,
                    format!("{:016x}", self.title_id()),
                    Some(self.title_version().to_string()),
                    IconHint::Builtin(self.depot_type()),
                );
                nand.record(&artifact).await?;
                Ok((artifact, installed))
            }
            .await;

            match result {
                Ok((artifact, paths)) => {
                    let _ = sender.send(Ok(artifact));
                    paths.into_iter().map(Ok).collect::<Vec<_>>()
                }
                Err(error) => {
                    let _ = sender.send(Err(error.clone()));
                    vec![Err(error)]
                }
            }
        })
        .map(stream::iter)
        .flatten();
        (
            Box::new(receiver.unwrap_or_else(|_| Err(Error::InstallIncomplete))),
            Box::new(operation),
        )
    }
}

#[cfg(test)]
mod tests {
    use aes::Aes128;
    use aes::cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray};
    use futures::StreamExt;
    use sha1::{Digest, Sha1};
    use uuid::Uuid;

    use super::*;
    use crate::impls::wii::nand::WiiNand;
    use crate::nand::VirtualNand;
    use crate::{PlatformRepository, StorageKind, VolumeStorage};

    const COMMON_KEY: [u8; 16] = [
        0xeb, 0xe4, 0x2a, 0x22, 0x5e, 0x85, 0x93, 0xe4, 0x48, 0xd9, 0xc5, 0x45, 0x73, 0x81, 0xaa,
        0xf7,
    ];

    #[tokio::test]
    async fn installs_a_valid_wad() {
        let source = tempfile::tempdir().unwrap();
        let wad_path = source.path().join("title.wad");
        let title_id = 0x0001_0001_5741_4445;
        let plaintext = b"polymer wii content";
        std::fs::write(&wad_path, make_wad(title_id, plaintext)).unwrap();
        let wad = WiiWadDepot::open(&wad_path).unwrap();
        assert_eq!(wad.title_id(), title_id);
        assert!(wad.validate_contents().is_empty());

        let incomplete_dir = tempfile::tempdir().unwrap();
        let incomplete_platform = PlatformRepository::open(
            incomplete_dir.path().join("NINTENDO_WII"),
            WiiNand::PLATFORM,
        )
        .unwrap();
        let incomplete_id = Uuid::now_v7();
        let incomplete = incomplete_platform.volume(StorageKind::Nand, incomplete_id);
        incomplete
            .brand(incomplete_id, WiiNand::PLATFORM, None)
            .await
            .unwrap();
        let incomplete = WiiNand::open(incomplete).await.unwrap();
        assert!(!wad.satisfies(&incomplete));

        let destination = tempfile::tempdir().unwrap();
        let platform = PlatformRepository::open(
            destination.path().join("NINTENDO_WII.git"),
            WiiNand::PLATFORM,
        )
        .unwrap();
        let id = Uuid::now_v7();
        let storage = platform.volume(StorageKind::Nand, id);
        let nand = WiiNand::new_empty(storage, id, None).await.unwrap();
        assert!(wad.satisfies(&nand));
        let (artifact, progress) = wad.install(&nand);
        let mut progress = Box::into_pin(progress);
        while let Some(path) = progress.as_mut().next().await {
            path.unwrap();
        }
        let artifact = Box::into_pin(artifact);
        let artifact = artifact.await.unwrap();
        assert_eq!(artifact.id(), format!("{title_id:016x}"));

        let path = "title/00010001/57414445/content/00000001.app";
        let mut installed = Vec::new();
        nand.root()
            .open_read(path)
            .unwrap()
            .read_to_end(&mut installed)
            .unwrap();
        assert_eq!(installed, plaintext);
        let mut uid_sys = Vec::new();
        nand.root()
            .open_read("sys/uid.sys")
            .unwrap()
            .read_to_end(&mut uid_sys)
            .unwrap();
        assert_eq!(
            uid_sys,
            [
                0x0000_0001_0000_0002_u64.to_be_bytes().as_slice(),
                0x1000_u32.to_be_bytes().as_slice(),
                title_id.to_be_bytes().as_slice(),
                0x1001_u32.to_be_bytes().as_slice(),
            ]
            .concat()
        );
        assert!(
            nand.root()
                .artifacts()
                .contains(artifact.id())
                .await
                .unwrap()
        );
        nand.uninstall(&[artifact.id()]).await.unwrap();
        assert!(!nand.root().exists(path));
        assert!(
            !nand
                .root()
                .artifacts()
                .contains(artifact.id())
                .await
                .unwrap()
        );
    }

    #[test]
    fn repairs_a_fakesigned_ticket_with_the_wrong_common_key_index() {
        let source = tempfile::tempdir().unwrap();
        let wad_path = source.path().join("wrong-key.wad");
        let mut bytes = make_wad(0x0001_0001_5741_4445, b"content");
        bytes[0x40 + 0x140 + 0xb1] = 1;
        std::fs::write(&wad_path, bytes).unwrap();
        let wad = WiiWadDepot::open(&wad_path).unwrap();
        assert!(wad.validate_contents().is_empty());
        assert_eq!(wad.select_title_key().unwrap().index, 0);
    }
    fn make_wad(title_id: u64, plaintext: &[u8]) -> Vec<u8> {
        make_wad_with_kind(title_id, plaintext, 1)
    }

    fn make_wad_with_kind(title_id: u64, plaintext: &[u8], kind: u16) -> Vec<u8> {
        let title_key = [0x5a; 16];
        let mut title_iv = [0; 16];
        title_iv[..8].copy_from_slice(&title_id.to_be_bytes());
        let encrypted_title_key = encrypt_cbc(&COMMON_KEY, &title_iv, &title_key);

        let mut ticket = vec![0; 0x2a4];
        ticket[..4].copy_from_slice(&0x0001_0001_u32.to_be_bytes());
        let body = 0x140;
        ticket[body + 0x7f..body + 0x8f].copy_from_slice(&encrypted_title_key);
        ticket[body + 0x9c..body + 0xa4].copy_from_slice(&title_id.to_be_bytes());
        ticket[body + 0xb1] = 0;

        let mut tmd = vec![0; 0x1e4 + 0x24];
        tmd[..4].copy_from_slice(&0x0001_0001_u32.to_be_bytes());
        let body = 0x140;
        tmd[body + 0x4c..body + 0x54].copy_from_slice(&title_id.to_be_bytes());
        tmd[body + 0x9c..body + 0x9e].copy_from_slice(&7_u16.to_be_bytes());
        tmd[body + 0x9e..body + 0xa0].copy_from_slice(&1_u16.to_be_bytes());
        let content = body + 0xa4;
        tmd[content..content + 4].copy_from_slice(&1_u32.to_be_bytes());
        tmd[content + 4..content + 6].copy_from_slice(&0_u16.to_be_bytes());
        tmd[content + 6..content + 8].copy_from_slice(&kind.to_be_bytes());
        tmd[content + 8..content + 16].copy_from_slice(&(plaintext.len() as u64).to_be_bytes());
        tmd[content + 16..content + 36].copy_from_slice(&Sha1::digest(plaintext));

        let mut padded = plaintext.to_vec();
        padded.resize(align_usize(padded.len(), 0x40), 0);
        let encrypted = encrypt_cbc(&title_key, &[0; 16], &padded);

        let ticket_offset = 0x40;
        let tmd_offset = align_usize(ticket_offset + ticket.len(), 0x40);
        let data_offset = align_usize(tmd_offset + tmd.len(), 0x40);
        let mut wad = vec![0; data_offset + encrypted.len()];
        wad[..4].copy_from_slice(&0x20_u32.to_be_bytes());
        wad[4..6].copy_from_slice(&0x4973_u16.to_be_bytes());
        wad[6..8].copy_from_slice(&0_u16.to_be_bytes());
        wad[0x10..0x14].copy_from_slice(&(ticket.len() as u32).to_be_bytes());
        wad[0x14..0x18].copy_from_slice(&(tmd.len() as u32).to_be_bytes());
        wad[0x18..0x1c].copy_from_slice(&(encrypted.len() as u32).to_be_bytes());
        wad[ticket_offset..ticket_offset + ticket.len()].copy_from_slice(&ticket);
        wad[tmd_offset..tmd_offset + tmd.len()].copy_from_slice(&tmd);
        wad[data_offset..].copy_from_slice(&encrypted);
        wad
    }

    fn encrypt_cbc(key: &[u8; 16], iv: &[u8; 16], bytes: &[u8]) -> Vec<u8> {
        let cipher = Aes128::new_from_slice(key).unwrap();
        let mut previous = *iv;
        let mut output = Vec::with_capacity(bytes.len());
        for plain in bytes.chunks_exact(16) {
            let mut block = [0; 16];
            for index in 0..16 {
                block[index] = plain[index] ^ previous[index];
            }
            let mut block = GenericArray::clone_from_slice(&block);
            cipher.encrypt_block(&mut block);
            output.extend_from_slice(&block);
            previous.copy_from_slice(&block);
        }
        output
    }

    fn align_usize(value: usize, alignment: usize) -> usize {
        (value + alignment - 1) & !(alignment - 1)
    }
}
