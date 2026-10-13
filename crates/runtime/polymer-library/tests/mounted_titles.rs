use std::{fs, io::Read, path::PathBuf, sync::Arc};

use polymer_consolestorage::impls::wiiu::nand::WiiUNand;
use polymer_consolestorage::installable::{DepotType, InPlaceTitle};
use polymer_consolestorage::nand::VirtualNand;
use polymer_consolestorage::{
    EntryKind, MountableStorage, PlatformRepository, StorageKind, TitleMount,
};
use polymer_library::keys::KeyStore;
use uuid::Uuid;

fn dump(root: &std::path::Path, id: u64, marker: &[u8]) -> PathBuf {
    let path = root.join(format!("{id:016x}"));
    fs::create_dir_all(path.join("code")).unwrap();
    fs::create_dir_all(path.join("content")).unwrap();
    fs::create_dir_all(path.join("meta")).unwrap();
    let base = 0x0005000000000000u64 | (id & 0xffffffff);
    fs::write(
        path.join("code/app.xml"),
        format!("<app><title_id>{id:016x}</title_id></app>"),
    )
    .unwrap();
    fs::write(
        path.join("meta/meta.xml"),
        format!("<menu><title_id>{base:016x}</title_id><title_version>1</title_version></menu>"),
    )
    .unwrap();
    fs::write(
        path.join("code/cos.xml"),
        "<app><argstr>main.rpx</argstr></app>",
    )
    .unwrap();
    fs::write(path.join("code/main.rpx"), marker).unwrap();
    path
}

fn read(storage: &dyn MountableStorage, path: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    storage
        .open_read(path)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    bytes
}

#[tokio::test]
async fn external_titles_mount_read_only_until_a_managed_install_wins() {
    let directory = tempfile::tempdir().unwrap();
    let repository =
        PlatformRepository::open(directory.path().join("platform"), WiiUNand::PLATFORM).unwrap();
    let id = Uuid::new_v4();
    let nand = WiiUNand::new_empty(repository.volume(StorageKind::Nand, id), id, None)
        .await
        .unwrap();
    let keys = Arc::new(
        KeyStore::open(directory.path().join("keys"))
            .await
            .unwrap()
            .for_platform(WiiUNand::PLATFORM)
            .unwrap(),
    );
    let external = directory.path().join("external");
    let base = InPlaceTitle::locate(&dump(&external, 0x0005000012345600, b"external")).unwrap();
    let update = InPlaceTitle::locate(&dump(&external, 0x0005000e12345600, b"update")).unwrap();
    assert_eq!(base.kind, DepotType::App);
    assert_eq!(update.kind, DepotType::AppUpdate);
    assert_eq!(base.title, update.title);
    assert_eq!(
        base.entrypoint.as_ref().unwrap().path,
        "code/main.rpx".to_owned()
    );
    let mounts = vec![
        TitleMount {
            install_path: base.install_path.clone(),
            host: external.join("0005000012345600"),
        },
        TitleMount {
            install_path: update.install_path.clone(),
            host: external.join("0005000e12345600"),
        },
    ];

    let composed = nand
        .compose_mounted(None, keys.clone(), mounts.clone())
        .await
        .unwrap();
    let rpx = format!("{}/code/main.rpx", base.install_path);
    assert_eq!(read(&*composed, &rpx), b"external");
    assert!(
        composed
            .read_dir("usr/title/00050000", false)
            .unwrap()
            .any(|entry| entry.name == "12345600")
    );
    assert!(composed.open(&rpx, true).is_err());
    assert!(
        composed
            .create(&format!("{}/content/new.bin", base.install_path))
            .is_err()
    );

    nand.try_install(
        &dump(
            &directory.path().join("managed"),
            0x0005000012345600,
            b"managed",
        ),
        &*keys,
    )
    .unwrap()
    .finish()
    .await
    .unwrap();
    let composed = nand.compose_mounted(None, keys, mounts).await.unwrap();
    assert_eq!(read(&*composed, &rpx), b"managed");
    assert_eq!(
        read(
            &*composed,
            &format!("{}/code/main.rpx", update.install_path)
        ),
        b"update"
    );
}

fn sfo(values: &[(&str, &str)]) -> Vec<u8> {
    let (mut keys, mut data, mut entries) = (Vec::new(), Vec::new(), Vec::new());
    for (key, value) in values {
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

fn tree(root: &std::path::Path, files: &[(&str, Vec<u8>)]) -> PathBuf {
    for (name, bytes) in files {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    root.to_owned()
}

#[test]
fn sony_title_directories_mount_at_their_console_paths() {
    let directory = tempfile::tempdir().unwrap();
    let hdd = sfo(&[("CATEGORY", "HG"), ("TITLE_ID", "NPUB30584")]);
    let eboot = ("USRDIR/EBOOT.BIN", b"elf".to_vec());

    let named = tree(
        &directory.path().join("NPUB30584"),
        &[("PARAM.SFO", hdd.clone()), eboot.clone()],
    );
    let title = InPlaceTitle::locate(&named).unwrap();
    assert_eq!(title.install_path, "dev_hdd0/game/NPUB30584");
    assert_eq!(title.entrypoint.unwrap().path, "USRDIR/EBOOT.BIN");

    let renamed = tree(
        &directory.path().join("packages"),
        &[("PARAM.SFO", hdd.clone()), eboot.clone()],
    );
    assert_eq!(
        InPlaceTitle::locate(&renamed).unwrap().install_path,
        "dev_hdd0/game/NPUB30584"
    );

    let disc = sfo(&[("CATEGORY", "DG"), ("TITLE_ID", "BLUS30443")]);
    let disc = tree(
        &directory.path().join("disc"),
        &[("PARAM.SFO", disc), eboot],
    );
    assert!(InPlaceTitle::locate(&disc).is_err());

    let vita = sfo(&[
        ("CATEGORY", "gd"),
        ("TITLE_ID", "PCSE00001"),
        ("CONTENT_ID", "UP0001-PCSE00001_00-0000000000000000"),
    ]);
    let app = tree(
        &directory.path().join("vita"),
        &[
            ("sce_sys/param.sfo", vita.clone()),
            ("eboot.bin", b"elf".to_vec()),
        ],
    );
    let title = InPlaceTitle::locate(&app).unwrap();
    assert_eq!(title.install_path, "ux0/app/PCSE00001");
    assert_eq!(title.entrypoint.unwrap().path, "eboot.bin");

    let incomplete = tree(
        &directory.path().join("incomplete"),
        &[("sce_sys/param.sfo", vita)],
    );
    assert!(InPlaceTitle::locate(&incomplete).is_err());
}

#[tokio::test]
async fn mounts_are_reachable_through_directories_the_nand_lacks() {
    let directory = tempfile::tempdir().unwrap();
    let repository =
        PlatformRepository::open(directory.path().join("platform"), WiiUNand::PLATFORM).unwrap();
    let id = Uuid::new_v4();
    let nand = WiiUNand::new_empty(repository.volume(StorageKind::Nand, id), id, None)
        .await
        .unwrap();
    let keys = Arc::new(
        KeyStore::open(directory.path().join("keys"))
            .await
            .unwrap()
            .for_platform(WiiUNand::PLATFORM)
            .unwrap(),
    );
    let host = dump(
        &directory.path().join("external"),
        0x0005000c12345600,
        b"addon",
    );
    let composed = nand
        .compose_mounted(
            None,
            keys,
            vec![TitleMount {
                install_path: "usr/extra/12345600/addon".to_owned(),
                host,
            }],
        )
        .await
        .unwrap();
    assert!(
        composed
            .read_dir("usr", false)
            .unwrap()
            .any(|entry| entry.name == "extra")
    );
    assert!(composed.metadata("usr/extra/12345600").unwrap().kind == EntryKind::Directory);
    assert_eq!(
        read(&*composed, "usr/extra/12345600/addon/code/main.rpx"),
        b"addon"
    );
    assert!(composed.create("usr/extra/12345600/addon/new.bin").is_err());
    assert!(composed.read_dir("usr/title/00050000", false).is_ok());
}

#[test]
fn install_paths_name_their_base_title() {
    assert_eq!(
        InPlaceTitle::title_at(WiiUNand::PLATFORM, "usr/title/0005000e/101c9400").as_deref(),
        Some("00050000101c9400")
    );
    assert_eq!(
        InPlaceTitle::title_at("SONY_PSV", "ux0/addcont/PCSE00001/DLC").as_deref(),
        Some("PCSE00001")
    );
    assert_eq!(
        InPlaceTitle::title_at("SONY_PS3", "dev_hdd0/game/NPUB30584").as_deref(),
        Some("NPUB30584")
    );
    assert_eq!(
        InPlaceTitle::title_at(WiiUNand::PLATFORM, "sys/title"),
        None
    );
}
