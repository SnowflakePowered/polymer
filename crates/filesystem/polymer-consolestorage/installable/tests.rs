use super::{DepotFile, DepotType, IconHint, artifacts::Artifact};
use crate::{PlatformRepository, Storage, StorageKind, VolumeStorage};
use std::{
    io::{Cursor, Read},
    path::PathBuf,
};
use uuid::Uuid;

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
