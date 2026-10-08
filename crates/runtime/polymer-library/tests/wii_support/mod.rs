use std::{fs, io::Read, path::PathBuf};

use aes::cipher::{BlockEncrypt, KeyInit};
use polymer_consolestorage::impls::wii::{WiiNand, ticket::Ticket};
use polymer_consolestorage::installable::artifacts::Artifact;
use polymer_consolestorage::nand::VirtualNand;
use polymer_consolestorage::{Error, GitVolumeStorage, PlatformRepository, Storage, StorageKind};
use polymer_library::keys::{KeyStore, PlatformKeys};
use sha1::{Digest, Sha1};
use uuid::Uuid;

pub struct WiiFixture {
    directory: tempfile::TempDir,
    pub platform: PlatformRepository,
    pub volume: GitVolumeStorage,
    pub nand: WiiNand,
    pub keys: PlatformKeys,
}

impl WiiFixture {
    pub async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let platform =
            PlatformRepository::open(directory.path().join("platform"), WiiNand::PLATFORM).unwrap();
        let id = Uuid::new_v4();
        let volume = platform.volume(StorageKind::Nand, id);
        let nand = WiiNand::new_empty(volume.clone(), id, None).await.unwrap();
        let keys = KeyStore::open(directory.path().join("keys"))
            .await
            .unwrap()
            .for_platform(WiiNand::PLATFORM)
            .unwrap();
        Self {
            directory,
            platform,
            volume,
            nand,
            keys,
        }
    }

    pub fn package(&self, wad: Wad) -> PathBuf {
        let path = self
            .directory
            .path()
            .join(format!("{}.wad", Uuid::new_v4()));
        fs::write(&path, wad.bytes()).unwrap();
        path
    }

    pub async fn install(&self, wad: Wad) -> Result<Artifact, Error> {
        self.nand
            .try_install(&self.package(wad), &self.keys)?
            .finish()
            .await
    }

    pub fn read(&self, path: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        self.volume
            .open_read(path)
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        bytes
    }

    pub fn shared_path(&self, bytes: &[u8]) -> Option<String> {
        let hash = Sha1::digest(bytes);
        self.read("shared1/content.map")
            .chunks_exact(28)
            .find(|entry| entry[8..] == hash[..])
            .map(|entry| format!("shared1/{}.app", std::str::from_utf8(&entry[..8]).unwrap()))
    }
}

pub struct Wad {
    title: u64,
    version: u16,
    pub ios: u64,
    contents: Vec<(u32, bool, Vec<u8>)>,
}

impl Wad {
    pub fn new(title: u64, version: u16) -> Self {
        Self {
            title,
            version,
            ios: 0,
            contents: vec![(0, false, b"executable".to_vec())],
        }
    }

    pub fn content(mut self, id: u32, shared: bool, bytes: &[u8]) -> Self {
        self.contents.push((id, shared, bytes.to_vec()));
        self
    }

    pub fn bytes(&self) -> Vec<u8> {
        let key = [0x42; 16];
        let ticket = Ticket::synthesize(self.title, &key).unwrap();
        let mut tmd = vec![0; 0x1e4];
        tmd[..4].copy_from_slice(&0x00010001u32.to_be_bytes());
        tmd[0x184..0x18c].copy_from_slice(&self.ios.to_be_bytes());
        tmd[0x18c..0x194].copy_from_slice(&self.title.to_be_bytes());
        tmd[0x1dc..0x1de].copy_from_slice(&self.version.to_be_bytes());
        tmd[0x1de..0x1e0].copy_from_slice(&(self.contents.len() as u16).to_be_bytes());
        let cipher = aes::Aes128::new(&key.into());
        let mut data = Vec::new();
        for (index, (id, shared, bytes)) in self.contents.iter().enumerate() {
            tmd.extend_from_slice(&id.to_be_bytes());
            tmd.extend_from_slice(&(index as u16).to_be_bytes());
            tmd.extend_from_slice(&(if *shared { 0x8001u16 } else { 1 }).to_be_bytes());
            tmd.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
            tmd.extend_from_slice(&Sha1::digest(bytes));
            let mut encrypted = bytes.clone();
            encrypted.resize(encrypted.len().next_multiple_of(64), 0);
            let mut previous = [0u8; 16];
            previous[..2].copy_from_slice(&(index as u16).to_be_bytes());
            for block in encrypted.chunks_exact_mut(16) {
                for (byte, chain) in block.iter_mut().zip(previous) {
                    *byte ^= chain;
                }
                cipher.encrypt_block(block.into());
                previous.copy_from_slice(block);
            }
            data.extend(encrypted);
        }
        let mut bytes = vec![0; 0x40];
        bytes[..4].copy_from_slice(&0x20u32.to_be_bytes());
        bytes[4..6].copy_from_slice(&0x4973u16.to_be_bytes());
        for (offset, length) in [(16, ticket.len()), (20, tmd.len()), (24, data.len())] {
            bytes[offset..offset + 4].copy_from_slice(&(length as u32).to_be_bytes());
        }
        for section in [ticket, tmd, data] {
            bytes.extend(section);
            bytes.resize(bytes.len().next_multiple_of(64), 0);
        }
        bytes
    }
}
