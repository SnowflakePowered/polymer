use super::{DepotFile, DepotType, IconHint, artifacts::Artifact};
use crate::{PlatformRepository, Storage, StorageKind, VolumeStorage};
use std::{
    io::{Cursor, Read},
    path::PathBuf,
};
use uuid::Uuid;

#[tokio::test]
async fn embedded_icons_are_metadata_and_replaced_with_the_artifact() {
    let directory = tempfile::tempdir().unwrap();
    let platform = PlatformRepository::open(directory.path().join("icons.git"), "TEST").unwrap();
    let id = Uuid::now_v7();
    let volume = platform.volume(StorageKind::Nand, id);
    volume.brand(id, "TEST", None).await.unwrap();
    let copied = volume
        .install_file(Box::new(Cursor::new(b"file")), "title/file")
        .await
        .unwrap();
    let manifest = vec![DepotFile::new(
        "title/file".into(),
        copied.blake3,
        copied.byte_length,
    )];
    let key = blake3::hash(b"title").to_hex();
    let path = format!("meta/artifacts/{key}/icon.png");
    let repo = platform.repository().to_thread_local();
    for pixel in [Some(10), Some(20), None] {
        let icon = match pixel {
            Some(value) => IconHint::image(image::RgbaImage::from_pixel(
                2,
                2,
                image::Rgba([value, 0, 0, 255]),
            ))
            .unwrap(),
            None => IconHint::Builtin(DepotType::App),
        };
        let artifact = Artifact::new(
            "title".into(),
            DepotType::App,
            manifest.clone(),
            "Title".into(),
            None,
            icon,
        );
        volume.artifacts().record(&artifact).await.unwrap();
        let reopened = platform.volume(StorageKind::Nand, id);
        let saved = reopened
            .artifacts()
            .artifact("title")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.byte_length(), 4);
        let revision =
            gix::ObjectId::from_hex(reopened.artifacts().revision().unwrap().as_bytes()).unwrap();
        let tree = repo.find_commit(revision).unwrap().tree().unwrap();
        let entry = tree.lookup_entry_by_path(&path).unwrap();
        match (pixel, saved.icon()) {
            (Some(value), IconHint::EmbeddedPng(bytes)) => {
                assert_eq!(
                    image::load_from_memory(bytes)
                        .unwrap()
                        .to_rgba8()
                        .get_pixel(0, 0)[0],
                    value
                );
                assert_eq!(
                    repo.find_blob(entry.unwrap().oid()).unwrap().data,
                    bytes.as_ref()
                );
            }
            (None, IconHint::Builtin(_)) => assert!(entry.is_none()),
            _ => panic!("icon did not round-trip"),
        }
        assert!(!reopened.exists(&path));
    }
    volume.artifacts().uninstall(&["title"]).await.unwrap();
    assert!(
        volume
            .artifacts()
            .artifact("title")
            .await
            .unwrap()
            .is_none()
    );
    assert!(!volume.exists("title/file"));
}

#[tokio::test]
async fn interrupted_install_remains_dirty_until_explicit_recovery() {
    let root = tempfile::tempdir().unwrap();
    let platform = PlatformRepository::open(root.path().join("platform.git"), "TEST").unwrap();
    let id = Uuid::now_v7();
    let volume = platform.volume(StorageKind::Nand, id);
    volume.brand(id, "TEST", None).await.unwrap();
    let written = volume
        .install_file(Box::new(Cursor::new(b"original")), "system/file")
        .await
        .unwrap();
    volume
        .artifacts()
        .record(&Artifact::new(
            "system".into(),
            DepotType::System,
            vec![DepotFile::new(
                PathBuf::from("system/file"),
                written.blake3,
                written.byte_length,
            )],
            "System".into(),
            None,
            IconHint::Builtin(DepotType::System),
        ))
        .await
        .unwrap();
    assert!(!volume.is_dirty().unwrap());
    let revision = volume.artifacts().revision().unwrap();
    volume
        .install_file(Box::new(Cursor::new(b"modified")), "system/file")
        .await
        .unwrap();
    volume
        .install_file(Box::new(Cursor::new(b"partial")), "system/new-file")
        .await
        .unwrap();
    // Lose the in-memory change list, as when the process exits mid-install.
    drop(volume);
    let recovered = platform.volume(StorageKind::Nand, id);
    assert!(recovered.is_dirty().unwrap());
    let mut bytes = Vec::new();
    recovered
        .open_read("system/file")
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes, b"modified");
    assert!(recovered.exists("system/new-file"));
    recovered.recover().unwrap();
    assert!(!recovered.is_dirty().unwrap());
    assert_eq!(
        std::fs::read(recovered.checkout_path().join("system/file")).unwrap(),
        b"original"
    );
    assert_eq!(recovered.artifacts().revision().unwrap(), revision);
    assert!(!recovered.exists("system/new-file"));
    assert_eq!(recovered.artifacts().summaries().await.unwrap().len(), 1);
}

#[tokio::test]
async fn dirty_checkout_detection_reads_disk_and_recovery_is_scoped_to_one_nand() {
    let root = tempfile::tempdir().unwrap();
    let platform = PlatformRepository::open(root.path().join("platform.git"), "TEST").unwrap();
    let id = Uuid::now_v7();
    let volume = platform.volume(StorageKind::Nand, id);
    volume.create_dir_all("empty").unwrap();
    volume
        .install_file(Box::new(Cursor::new(b"original")), "system/file")
        .await
        .unwrap();
    volume.brand(id, "TEST", None).await.unwrap();
    let other = platform.volume(StorageKind::Nand, Uuid::now_v7());
    other
        .install_file(Box::new(Cursor::new(b"other")), "keep")
        .await
        .unwrap();
    other.brand(other.id(), "TEST", None).await.unwrap();
    let checkout = volume.checkout_path().to_owned();
    let revision = volume.artifacts().revision().unwrap();
    drop(volume);

    for change in [
        "same size and timestamp",
        "deleted file",
        "untracked file",
        "untracked directory",
        "deleted directory",
    ] {
        let file = checkout.join("system/file");
        match change {
            "same size and timestamp" => {
                let modified = std::fs::metadata(&file).unwrap().modified().unwrap();
                std::fs::write(&file, b"modified").unwrap();
                std::fs::File::options()
                    .write(true)
                    .open(&file)
                    .unwrap()
                    .set_times(std::fs::FileTimes::new().set_modified(modified))
                    .unwrap();
            }
            "deleted file" => std::fs::remove_file(&file).unwrap(),
            "untracked file" => std::fs::write(checkout.join("unexpected"), b"partial").unwrap(),
            "untracked directory" => std::fs::create_dir(checkout.join("unexpected")).unwrap(),
            "deleted directory" => std::fs::remove_dir(checkout.join("empty")).unwrap(),
            _ => unreachable!(),
        }
        let reopened = platform.volume(StorageKind::Nand, id);
        // Ordinary reads must not repair deleted files or directories either.
        let _ = reopened.metadata("system/file");
        assert!(reopened.is_dirty().unwrap(), "{change}");
        reopened.recover().unwrap();
        assert!(!reopened.is_dirty().unwrap(), "{change}");
        assert_eq!(std::fs::read(&file).unwrap(), b"original");
        assert!(checkout.join("empty").is_dir());
        assert!(!checkout.join("unexpected").exists());
        assert_eq!(reopened.artifacts().revision().unwrap(), revision);
        assert_eq!(
            std::fs::read(other.checkout_path().join("keep")).unwrap(),
            b"other"
        );
        assert!(!other.is_dirty().unwrap());
    }
}
