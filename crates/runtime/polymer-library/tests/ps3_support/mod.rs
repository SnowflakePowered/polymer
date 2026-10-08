use std::{fs, io::Read, path::PathBuf, sync::Arc};

use aes::cipher::{KeyIvInit, StreamCipher};
use polymer_consolestorage::impls::ps3::Ps3Nand;
use polymer_consolestorage::installable::{
    DepotFile, DepotType, InstallContext, artifacts::Artifact,
};
use polymer_consolestorage::nand::VirtualNand;
use polymer_consolestorage::{
    ArtifactComponent, DirectoryStorage, Error, GitVolumeStorage, PlatformRepository, Storage,
    StorageKind, VolumeStorage,
};
use polymer_library::keys::{KeyStore, PlatformKeys};
use sha1::{Digest, Sha1};
use uuid::Uuid;

pub struct Ps3Fixture {
    directory: tempfile::TempDir,
    pub volume: GitVolumeStorage,
    pub nand: Ps3Nand,
    pub keys: PlatformKeys,
}

impl Ps3Fixture {
    pub async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let platform =
            PlatformRepository::open(directory.path().join("platform"), Ps3Nand::PLATFORM).unwrap();
        let id = Uuid::new_v4();
        let volume = platform.volume(StorageKind::Nand, id);
        let nand = Ps3Nand::new_empty(volume.clone(), id, None).await.unwrap();
        let keys = KeyStore::open(directory.path().join("keys"))
            .await
            .unwrap()
            .for_platform(Ps3Nand::PLATFORM)
            .unwrap();
        let fixture = Self {
            directory,
            volume,
            nand,
            keys,
        };
        fixture.firmware("4.90", true).await;
        fixture
    }

    pub async fn firmware(&self, version: &str, obsolete: bool) {
        let stage = tempfile::tempdir().unwrap();
        let mut files = vec![("dev_flash/sys/external/liblv2.sprx", version.as_bytes())];
        if obsolete {
            files.push(("dev_flash/obsolete", b"old"));
        }
        let manifest = files
            .into_iter()
            .map(|(path, bytes)| {
                fs::create_dir_all(stage.path().join(path).parent().unwrap()).unwrap();
                fs::write(stage.path().join(path), bytes).unwrap();
                DepotFile::new(
                    path.into(),
                    *blake3::hash(bytes).as_bytes(),
                    bytes.len() as u64,
                )
            })
            .collect();
        let artifact = Artifact::new(
            "ps3:firmware".into(),
            DepotType::System,
            manifest,
            "Firmware".into(),
            Some(version.into()),
            vec![],
            None,
        )
        .unwrap();
        let component = ArtifactComponent {
            id: "ps3:firmware".into(),
            title: "ps3:firmware".into(),
            root: "dev_flash".into(),
        };
        self.volume
            .artifacts()
            .install_component(
                &component,
                &serde_json::Value::Null,
                &artifact,
                Arc::new(DirectoryStorage::new_unmanaged(stage.path()).await.unwrap()),
                &InstallContext::default(),
            )
            .await
            .unwrap();
    }

    pub async fn install(&self, package: Package) -> Result<Artifact, Error> {
        let path = self.package(package);
        self.nand.try_install(&path, &self.keys)?.finish().await
    }

    pub fn package(&self, package: Package) -> PathBuf {
        let path = self
            .directory
            .path()
            .join(format!("{}.pkg", Uuid::new_v4()));
        package.write(&path);
        path
    }

    pub fn read(&self, name: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        self.volume
            .open_read(&format!("dev_hdd0/game/NPUB00001/{name}"))
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        bytes
    }
}

pub struct Package {
    kind: DepotType,
    pub suffix: &'static str,
    pub version: Option<&'static str>,
    pub target: Option<&'static str>,
    pub sfo_overwrite: bool,
    files: Vec<(&'static str, Vec<u8>, bool)>,
}

impl Package {
    pub fn base(version: &'static str) -> Self {
        Self {
            kind: DepotType::App,
            suffix: "BASE000000000000",
            version: Some(version),
            target: None,
            sfo_overwrite: true,
            files: vec![("USRDIR/EBOOT.BIN", b"executable".to_vec(), true)],
        }
    }

    pub fn update(version: &'static str) -> Self {
        Self {
            kind: DepotType::AppUpdate,
            files: vec![],
            ..Self::base(version)
        }
    }

    pub fn dlc(suffix: &'static str) -> Self {
        Self {
            kind: DepotType::AppContent,
            suffix,
            version: None,
            files: vec![],
            ..Self::base("01.00")
        }
    }

    pub fn file(mut self, path: &'static str, bytes: &[u8], overwrite: bool) -> Self {
        self.files.push((path, bytes.to_vec(), overwrite));
        self
    }

    fn sfo(&self) -> Vec<u8> {
        let mut values = vec![("TITLE_ID", "NPUB00001"), ("TITLE", "Fixture")];
        if let Some(version) = self.version {
            values.push(("APP_VER", version));
        }
        if let Some(target) = self.target {
            values.push(("TARGET_APP_VER", target));
        }
        let mut keys = Vec::new();
        let mut data = Vec::new();
        let mut entries = Vec::new();
        for (key, value) in &values {
            entries.extend_from_slice(&(keys.len() as u16).to_le_bytes());
            entries.extend_from_slice(&0x0204u16.to_le_bytes());
            let length = value.len() + 1;
            let capacity = length.next_multiple_of(4);
            for number in [length, capacity, data.len()] {
                entries.extend_from_slice(&(number as u32).to_le_bytes());
            }
            keys.extend_from_slice(key.as_bytes());
            keys.push(0);
            data.extend_from_slice(value.as_bytes());
            data.resize(data.len() + capacity - value.len(), 0);
        }
        let key_offset = 20 + entries.len();
        let data_offset = (key_offset + keys.len()).next_multiple_of(4);
        let mut bytes = Vec::new();
        for value in [
            0x46535000u32,
            0x101,
            key_offset as u32,
            data_offset as u32,
            values.len() as u32,
        ] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend(entries);
        bytes.extend(keys);
        bytes.resize(data_offset, 0);
        bytes.extend(data);
        bytes
    }

    fn write(mut self, path: &PathBuf) {
        if self.version.is_some() || self.target.is_some() {
            self.files
                .push(("PARAM.SFO", self.sfo(), self.sfo_overwrite));
        }
        let mut payload = vec![0; self.files.len() * 32];
        for (index, (name, bytes, overwrite)) in self.files.iter().enumerate() {
            let name_offset = payload.len();
            payload.extend_from_slice(name.as_bytes());
            payload.resize(payload.len().next_multiple_of(16), 0);
            let file_offset = payload.len();
            payload.extend_from_slice(bytes);
            payload.resize(payload.len().next_multiple_of(16), 0);
            let entry = &mut payload[index * 32..][..32];
            entry[0..4].copy_from_slice(&(name_offset as u32).to_be_bytes());
            entry[4..8].copy_from_slice(&(name.len() as u32).to_be_bytes());
            entry[8..16].copy_from_slice(&(file_offset as u64).to_be_bytes());
            entry[16..24].copy_from_slice(&(bytes.len() as u64).to_be_bytes());
            let flags = 3u32 | if *overwrite { 0x80000000 } else { 0 };
            entry[24..28].copy_from_slice(&flags.to_be_bytes());
        }
        let key = [
            0x2e, 0x7b, 0x71, 0xd7, 0xc9, 0xc9, 0xa1, 0x4e, 0xa3, 0x22, 0x1f, 0x18, 0x88, 0x28,
            0xb8, 0xf8,
        ];
        ctr::Ctr128BE::<aes::Aes128>::new(&key.into(), &[0; 16].into())
            .apply_keystream(&mut payload);
        let mut bytes = vec![0; 0x200];
        bytes[..8].copy_from_slice(&[0x7f, b'P', b'K', b'G', 0x80, 0, 0, 1]);
        for (offset, value) in [
            (8, 0xc0u32),
            (12, 2),
            (16, 24),
            (20, self.files.len() as u32),
            (0x24, 0x200),
        ] {
            bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
        }
        bytes[0x18..0x20].copy_from_slice(&((0x200 + payload.len() + 32) as u64).to_be_bytes());
        bytes[0x28..0x30].copy_from_slice(&(payload.len() as u64).to_be_bytes());
        let content_id = format!("EP0000-NPUB00001_00-{}", self.suffix);
        bytes[0x30..0x54].copy_from_slice(content_id.as_bytes());
        let content_type = if self.kind == DepotType::AppContent {
            4u32
        } else {
            5
        };
        let update = if self.kind == DepotType::AppUpdate {
            0x10u32
        } else {
            0
        };
        for (index, value) in [2u32, 4, content_type, 3, 4, update].iter().enumerate() {
            bytes[0xc0 + index * 4..0xc4 + index * 4].copy_from_slice(&value.to_be_bytes());
        }
        bytes.extend(payload);
        let digest = Sha1::digest(&bytes);
        bytes.extend_from_slice(&digest);
        bytes.extend_from_slice(&[0; 12]);
        fs::write(path, bytes).unwrap();
    }
}
