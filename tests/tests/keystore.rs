use polymer_consolestorage::keys::{KeyIdentity, KeyQuery, KeyReader, KeyRecord, KeyWriter};
use polymer_library::keys::KeyStore;

#[tokio::test]
async fn platform_keys_persist_atomically() {
    let directory = tempfile::tempdir().unwrap();
    let store = KeyStore::open(directory.path()).await.unwrap();
    let wii = store.for_platform("NINTENDO_WII").unwrap();
    let ps3 = store.for_platform("SONY_PS3").unwrap();
    let identity = KeyIdentity {
        namespace: "content".into(),
        id: "0001000148414241".into(),
    };
    let record = KeyRecord {
        identity: identity.clone(),
        title_id: Some(identity.id.clone()),
        bytes: vec![1; 16],
    };
    wii.put(&[record.clone()]).await.unwrap();
    assert!(ps3.list(&KeyQuery::default()).await.unwrap().is_empty());
    assert_eq!(
        wii.list(&KeyQuery {
            namespace: None,
            title_id: Some(identity.id.clone())
        })
        .await
        .unwrap()
        .len(),
        1
    );

    let replacement = KeyRecord {
        bytes: vec![2; 16],
        ..record.clone()
    };
    assert!(wii.put(&[replacement.clone(), replacement]).await.is_err());
    assert_eq!(wii.read(&identity).await.unwrap().bytes, record.bytes);
    wii.put(&[KeyRecord {
        bytes: vec![2; 16],
        ..record
    }])
    .await
    .unwrap();
    assert_eq!(wii.read(&identity).await.unwrap().bytes, vec![2; 16]);
    assert!(wii.remove(&identity).await.unwrap());
    assert!(!wii.remove(&identity).await.unwrap());
}

#[tokio::test]
async fn generated_licenses_compose_with_nand_and_saves_and_keep_session_bytes() {
    use polymer_library::{ConsoleStorage, StorageKind, StorageManager};
    use std::io::Read;
    let directory = tempfile::tempdir().unwrap();
    let manager = StorageManager::new(directory.path());
    let store = KeyStore::open(directory.path().join("keys")).await.unwrap();
    for (platform, id, target) in [
        (
            "NINTENDO_WII",
            "0001000148414241",
            "ticket/00010001/48414241.tik",
        ),
        (
            "NINTENDO_3DS",
            "0004008C00012300",
            "nand/dbs/ticket.db/0004008C00012300.0004008C00012300.tik",
        ),
        (
            "SONY_PS3",
            "UP0001-NPUB12345_00-0123456789ABCDEF",
            "dev_hdd0/home/00000001/exdata/UP0001-NPUB12345_00-0123456789ABCDEF.rap",
        ),
    ] {
        let keys = store.for_platform(platform).unwrap();
        let record = KeyRecord {
            identity: KeyIdentity {
                namespace: "content".into(),
                id: id.into(),
            },
            title_id: Some(id.into()),
            bytes: vec![0x42; 16],
        };
        keys.put(&[record.clone()]).await.unwrap();
        // Unassociated opaque records must not crash native ID selection.
        keys.put(&[KeyRecord {
            identity: KeyIdentity {
                namespace: "content".into(),
                id: "x".into(),
            },
            title_id: None,
            bytes: vec![1],
        }])
        .await
        .unwrap();
        let ConsoleStorage::Nand(nand) = manager
            .create(StorageKind::Nand, platform, None)
            .await
            .unwrap()
        else {
            panic!("expected NAND")
        };
        let ConsoleStorage::SaveRepo(saves) = manager
            .create(StorageKind::SaveRepo, platform, None)
            .await
            .unwrap()
        else {
            panic!("expected saves")
        };
        let parent = target.rsplit_once('/').unwrap().0;
        let sibling = format!("{parent}/unrelated.bin");
        nand.root()
            .create(&sibling)
            .unwrap()
            .write_all_at(0, b"existing content")
            .unwrap();
        nand.set_label(Some("with content")).await.unwrap();
        for selection in [None, Some(saves.clone())] {
            let composed = nand
                .compose(selection, std::sync::Arc::new(keys.clone()))
                .await
                .unwrap();
            let paths: Vec<_> = composed
                .read_dir("", true)
                .unwrap()
                .map(|entry| entry.path)
                .collect();
            assert!(
                paths.contains(&target.to_owned()),
                "{platform}: generated directory is hidden"
            );
            assert!(
                paths.contains(&sibling),
                "{platform}: unrelated content is hidden"
            );
            let mut ticket = Vec::new();
            composed
                .open_read(target)
                .unwrap()
                .read_to_end(&mut ticket)
                .unwrap();
            let decoded = match platform {
                "NINTENDO_WII" => {
                    polymer_consolestorage::impls::wii::ticket::Ticket::parse(ticket.clone())
                        .unwrap()
                        .key_record()
                        .unwrap()
                        .bytes
                }
                "NINTENDO_3DS" => {
                    polymer_consolestorage::impls::ctr::ticket::CtrTicket::decode(&ticket)
                        .unwrap()
                        .bytes
                }
                _ => ticket.clone(),
            };
            assert_eq!(decoded, record.bytes);
            keys.remove(&record.identity).await.unwrap();
            let mut retained = Vec::new();
            composed
                .open_read(target)
                .unwrap()
                .read_to_end(&mut retained)
                .unwrap();
            assert_eq!(retained, ticket);
            keys.put(&[record.clone()]).await.unwrap();
            composed
                .open(target, true)
                .unwrap()
                .write_at(0, b"overwrite")
                .unwrap();
            assert_eq!(
                keys.read(&record.identity).await.unwrap().bytes,
                record.bytes
            );
            assert!(!nand.is_dirty().unwrap());
        }
    }
}

#[tokio::test]
async fn replaced_keys_drop_stale_titles_and_ids_are_case_sensitive() {
    let directory = tempfile::tempdir().unwrap();
    let store = KeyStore::open(directory.path()).await.unwrap();
    let keys = store.for_platform("NINTENDO_WII").unwrap();
    let original = KeyRecord {
        identity: KeyIdentity {
            namespace: "system".into(),
            id: "CaseSensitive".into(),
        },
        title_id: Some("old".into()),
        bytes: vec![1],
    };
    keys.put(&[original.clone()]).await.unwrap();
    let replacement = KeyRecord {
        title_id: None,
        bytes: vec![2],
        ..original.clone()
    };
    keys.put(&[replacement]).await.unwrap();
    assert!(
        keys.list(&KeyQuery {
            namespace: None,
            title_id: Some("old".into())
        })
        .await
        .unwrap()
        .is_empty()
    );
    assert_eq!(
        keys.list(&KeyQuery::default()).await.unwrap()[0].title_id,
        None
    );
    assert!(
        keys.read(&KeyIdentity {
            namespace: "system".into(),
            id: "casesensitive".into()
        })
        .await
        .is_err()
    );
}

#[test]
fn generated_tickets_grant_high_content_indices_and_reject_missing_device_material() {
    use polymer_consolestorage::impls::{
        ctr::ticket::CtrTicket, wii::ticket::Ticket, wiiu::ticket::WiiUTicket,
    };
    let key = [0x42; 16];
    let mut ctr = CtrTicket::synthesize(0x0004008c00012300, &key).unwrap();
    // Interpret the content-index table the way Azahar's ContentIndex does:
    // each rights field covers 1024 bits beginning at its native start index.
    // See the pinned ticket.cpp reference in KEYSTORE_PLAN.md.
    let index = &ctr[0x2a4..];
    let header_offset = u32::from_be_bytes(index[8..12].try_into().unwrap()) as usize;
    let header = &index[header_offset..];
    let offset = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
    let count = u32::from_be_bytes(header[4..8].try_into().unwrap()) as usize;
    let stride = u32::from_be_bytes(header[8..12].try_into().unwrap()) as usize;
    for content in [0u16, 255, 256, 1023, 1024, 32768, 65535] {
        assert!(
            (0..count).any(|n| {
                let field = &index[offset + n * stride..];
                let base = u16::from_be_bytes(field[2..4].try_into().unwrap());
                let Some(bit) = content.checked_sub(base).filter(|bit| *bit < 1024) else {
                    return false;
                };
                field[4 + usize::from(bit / 8)] & (1 << (bit % 8)) != 0
            }),
            "3DS content index {content} is not licensed"
        );
    }
    assert_eq!(CtrTicket::decode(&ctr).unwrap().bytes, key);
    ctr[0x1d8..0x1dc].copy_from_slice(&1u32.to_be_bytes());
    assert!(
        CtrTicket::decode(&ctr)
            .unwrap_err()
            .to_string()
            .contains("personalized")
    );
    let mut wii = Ticket::synthesize(0x0001000148414241, &key).unwrap();
    wii[0x1d8..0x1dc].copy_from_slice(&1u32.to_be_bytes());
    assert!(
        Ticket::parse(wii)
            .err()
            .unwrap()
            .to_string()
            .contains("personalized")
    );
    let mut wiiu = WiiUTicket::synthesize(0x0005000c12345678, 7, &key).unwrap();
    assert_eq!(WiiUTicket::decode(&wiiu).unwrap().bytes, key);
    let ext = &wiiu[0x2a4..];
    let section = u32::from_be_bytes(ext[8..12].try_into().unwrap()) as usize;
    let rights = u32::from_be_bytes(ext[section..section + 4].try_into().unwrap()) as usize;
    assert_eq!(
        u32::from_be_bytes(ext[rights..rights + 4].try_into().unwrap()),
        0
    );
    for content in [0usize, 255, 256, 1023] {
        assert_ne!(ext[rights + 4 + content / 8] & (1 << (content % 8)), 0);
    }
    wiiu[0x1d8..0x1dc].copy_from_slice(&1u32.to_be_bytes());
    assert!(
        WiiUTicket::decode(&wiiu)
            .unwrap_err()
            .to_string()
            .contains("personalized")
    );
}
