use polymer_consolestorage::{StorageKind, impls::ps1::PlaystationMemoryCard};
use polymer_library::LaunchableUri;
use polymer_pluginhost::{
    loading::PluginRegistry,
    runtime::{PluginRuntime, PluginRuntimeOptions},
};
use polymer_scraping::Package;
use std::{collections::BTreeMap, sync::Arc};

#[tokio::test(flavor = "current_thread")]
async fn package_mounts_images_directories_and_cues_read_only() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("game.iso"), b"game").unwrap();
    std::fs::create_dir(root.path().join("assets")).unwrap();
    std::fs::write(root.path().join("assets/resource.bin"), b"asset").unwrap();
    std::fs::create_dir(root.path().join("assets/nested")).unwrap();
    std::fs::write(root.path().join("assets/nested/data.bin"), b"nested").unwrap();
    std::fs::write(
        root.path().join("plugin.toml"),
        r#"
        [plugin]
        id = "worker-test"
        version = "1"
        class = "emulator"
        main = "plugin.js"
        [[plugin.caches]]
        key = "shaders"
        label = "Shaders"
        [emulator]
        platforms = ["TEST"]
        [emulator.configuration]
        definitions = ["config.toml"]
    "#,
    )
    .unwrap();
    std::fs::write(
        root.path().join("config.toml"),
        r#"
        [targets.settings]
        output = { format = "ini", path = "settings.ini" }
        [input]
        driver = "sdl3"
        device = "SDL-{index}"
        [input.mapping]
        [input.layouts]
        [platforms.TEST.ports]
    "#,
    )
    .unwrap();
    std::fs::write(root.path().join("plugin.js"), r#"
        import assets from "pkg:assets";
        import image from "pkg:game.iso";
        import { caches } from "polymer:cache";
        export async function onLaunch(args) {
            args.filesystem.mount("/ScratchDefault");
            args.filesystem.mount("/ScratchOptions", {});
            args.filesystem.mount("/ScratchReadOnly", { mode: "rx" });
            for (const source of [assets, image, new Uint8Array(), args.package]) {
                let rejected = false;
                try { args.filesystem.mount("/InvalidFormat", source, { mode: "r", format: "raw" }); }
                catch { rejected = true; }
                if (!rejected) throw Error("format accepted for a non-storage source");
            }
            if (args.storage.card) {
                args.filesystem.mount("/DefaultCard.mcd", args.storage.card, { mode: "r" });
                args.filesystem.mount("/ExplicitCard.mcd", args.storage.card, { mode: "r", format: "raw" });
            }
            args.filesystem.mount("/Imported.iso", image, { mode: "r" });
            args.filesystem.mount("/Assets", assets, { mode: "rx" });
            for await (const entry of assets) {
                args.filesystem.mount(`/Selected/${entry.name}`, entry, { mode: "rx" });
                if (entry.kind === "directory") {
                    for await (const file of entry) args.filesystem.mount(`/Deep/${file.name}`, file, { mode: "r" });
                }
                let denied = false;
                try { args.filesystem.mount(`/Writable/${entry.name}`, entry, { mode: "rwx" }); }
                catch { denied = true; }
                if (!denied) throw Error("package entry granted write access");
            }
            const writer = caches.shaders.open("nested/shader.bin", { write: true }).writable.getWriter();
            await writer.write(new TextEncoder().encode("shader"));
            await writer.close();
            args.filesystem.mount("/Shaders", caches.shaders, { mode: "rx" });
            for await (const entry of caches.shaders) {
                if (entry.kind === "directory") args.filesystem.mount("/WritableShaders", entry, { mode: "rwx" });
            }
            let denied = false;
            try { args.filesystem.mount("/WritableGame", args.package, { mode: "rw" }); }
            catch { denied = true; }
            if (!denied) throw Error("package granted write access");
            // Changing the launch path cannot redirect the bound package.
            args.launchPath = "not-the-game";
            const gamePath = args.filesystem.mount("/Game", args.package, { mode: "rx" });
            const command = args.filesystem.mount("/Emulator/emulator.exe", new Uint8Array([1]), { mode: "rx" });
            return { command, argv: [gamePath],
                hooks: { graphics: ["vulkan"], input: [] }, surfaces: [{ index: 0 }] };
        }
    "#).unwrap();
    let registry = PluginRegistry::new();
    registry.load(root.path()).unwrap();
    let storage = tempfile::tempdir().unwrap();
    let card = polymer_library::StorageManager::new(storage.path())
        .create(
            StorageKind::MemoryCard,
            PlaystationMemoryCard::PLATFORM,
            None,
        )
        .await
        .unwrap();
    let card_id = card.metadata().await.unwrap().id;
    let storage_mounts: polymer_pluginhost::filesystem::StorageFactories = BTreeMap::from([(
        "card".to_owned(),
        Arc::new(polymer_vfs::MemoryCardStorageFactory::new(
            card.downcast::<PlaystationMemoryCard>().unwrap(),
            Vec::new(),
            card_id,
        )) as Arc<dyn polymer_vfs::ResourceFactory<dyn polymer_vfs::Volume>>,
    )]);
    let runtime = PluginRuntime::new(
        registry,
        polymer_library::Directories::rooted(storage.path(), storage.path()),
        PluginRuntimeOptions::default(),
    );
    // A minimal Games Express PC Engine CD track recognized by Gumshoe.
    // Use an original filename that must be rewritten for the mounted bundle.
    let mut track = vec![0; 17 * 2048];
    track[16 * 2048 + 8..16 * 2048 + 28].copy_from_slice(b"HACKER CD ROM SYSTEM");
    std::fs::write(root.path().join("original track.bin"), &track).unwrap();
    let source = root.path().join("source.cue");
    std::fs::write(
        &source,
        "FILE \"original track.bin\" BINARY\n  TRACK 01 MODE1/2048\n    INDEX 01 00:00:00\n",
    )
    .unwrap();
    let identified = Package::identify(&source, Some("NEC_TG16"), None).unwrap();
    // Prepare a package after its manifest passes through the frontend round trip.
    let mut package: Package =
        serde_json::from_value(serde_json::to_value(identified).unwrap()).unwrap();
    package.prepare_launch().unwrap();
    assert_eq!(
        package.binary_digest,
        blake3::hash(&track).to_hex().as_str()
    );

    // Prepared packages can be prepared again after another frontend round trip.
    let mut package: Package =
        serde_json::from_value(serde_json::to_value(package).unwrap()).unwrap();
    package.prepare_launch().unwrap();
    // Mounting uses the generated descriptor, not the original CUE file.
    std::fs::remove_file(&source).unwrap();
    let cue = "FILE \"file-0000.bin\" BINARY\n  TRACK 01 MODE1/2048\n    INDEX 01 00:00:00\n";
    let result = runtime
        .handle()
        .invoke_with_storage_owned(
            "worker-test",
            &["onLaunch".to_owned()],
            vec![serde_json::json!({
                "launchPath": root.path().join("source.cue"), "console": "TEST",
                "controllerTopology": { "mappings": [] }, "virtualize": true,
                "package": package,
            })],
            storage_mounts,
        )
        .await
        .unwrap();
    assert!(
        result.value["argv"][0]
            .as_str()
            .unwrap()
            .ends_with("disc.cue")
    );
    let prepared = result.prepared_filesystem.unwrap();
    let filesystem = prepared.filesystem();
    filesystem.materialize().unwrap();
    for parent in ["/ScratchDefault", "/ScratchOptions"] {
        let path = format!("{parent}/created.bin").parse().unwrap();
        filesystem
            .create_file(&path, polymer_vfs::CreateOptions::default())
            .unwrap();
        let file = filesystem
            .open(
                &path,
                polymer_vfs::OpenOptions {
                    read: true,
                    write: true,
                    truncate: false,
                },
            )
            .unwrap();
        file.write(0, b"scratch").unwrap();
        let mut bytes = [0; 7];
        file.read(0, &mut bytes).unwrap();
        assert_eq!(&bytes, b"scratch");
    }
    assert!(matches!(
        filesystem.create_file(
            &"/ScratchReadOnly/denied.bin".parse().unwrap(),
            polymer_vfs::CreateOptions::default(),
        ),
        Err(polymer_vfs::VfsError::PermissionDenied(_))
    ));
    for (name, expected) in [
        ("/Game/disc.cue", cue.as_bytes()),
        ("/Game/file-0000.bin", track.as_slice()),
        ("/Assets/resource.bin", b"asset".as_slice()),
        ("/Imported.iso", b"game".as_slice()),
        ("/DefaultCard.mcd", b"MC".as_slice()),
        ("/ExplicitCard.mcd", b"MC".as_slice()),
        ("/Selected/resource.bin", b"asset".as_slice()),
        ("/Selected/nested/data.bin", b"nested".as_slice()),
        ("/Deep/data.bin", b"nested".as_slice()),
        ("/Shaders/nested/shader.bin", b"shader".as_slice()),
    ] {
        let path = name.parse().unwrap();
        let file = filesystem
            .open(
                &path,
                polymer_vfs::OpenOptions {
                    read: true,
                    write: false,
                    truncate: false,
                },
            )
            .unwrap();
        let mut bytes = vec![0; expected.len()];
        let length = file.read(0, &mut bytes).unwrap();
        assert_eq!(&bytes[..length], expected);
        assert!(
            filesystem
                .open(
                    &path,
                    polymer_vfs::OpenOptions {
                        read: true,
                        write: true,
                        truncate: false
                    }
                )
                .is_err()
        );
    }
    assert!(
        filesystem
            .open(
                &"/Game/game.iso".parse().unwrap(),
                polymer_vfs::OpenOptions {
                    read: true,
                    write: false,
                    truncate: false
                }
            )
            .is_err()
    );
    filesystem
        .rename(
            &"/WritableShaders/shader.bin".parse().unwrap(),
            &"/WritableShaders/renamed.bin".parse().unwrap(),
        )
        .unwrap();
    let renamed = filesystem
        .open(
            &"/Shaders/nested/renamed.bin".parse().unwrap(),
            polymer_vfs::OpenOptions {
                read: true,
                write: false,
                truncate: false,
            },
        )
        .unwrap();
    let mut bytes = [0; 6];
    renamed.read(0, &mut bytes).unwrap();
    assert_eq!(&bytes, b"shader");
    drop(renamed);
    drop(prepared);
    std::fs::write(root.path().join("standalone.iso"), b"game").unwrap();
    std::fs::write(root.path().join("rom.gba"), b"cartridge").unwrap();
    std::fs::create_dir_all(root.path().join("extracted/code")).unwrap();
    std::fs::write(root.path().join("extracted/code/main.rpx"), b"executable").unwrap();
    for (source, entrypoint, member, expected) in [
        (
            "standalone.iso",
            "/Game/standalone.iso",
            "/Game/standalone.iso",
            b"game".as_slice(),
        ),
        (
            "rom.gba",
            "/Game/rom.gba",
            "/Game/rom.gba",
            b"cartridge".as_slice(),
        ),
        (
            "extracted",
            "/Game",
            "/Game/code/main.rpx",
            b"executable".as_slice(),
        ),
    ] {
        let result = runtime
            .handle()
            .invoke_with_storage_owned(
                "worker-test",
                &["onLaunch".to_owned()],
                vec![serde_json::json!({
                    "launchPath": root.path().join(source), "console": "TEST",
                    "package": { "uri": LaunchableUri::file(root.path().join(source)).unwrap() }, "controllerTopology": { "mappings": [] },
                })],
                BTreeMap::new(),
            )
            .await
            .unwrap();
        assert!(
            result.value["argv"][0]
                .as_str()
                .unwrap()
                .replace('\\', "/")
                .ends_with(entrypoint)
        );
        let prepared = result.prepared_filesystem.unwrap();
        let filesystem = prepared.filesystem();
        filesystem.materialize().unwrap();
        let path = member.parse().unwrap();
        let file = filesystem
            .open(
                &path,
                polymer_vfs::OpenOptions {
                    read: true,
                    write: false,
                    truncate: false,
                },
            )
            .unwrap();
        let mut bytes = [0; 256];
        let length = file.read(0, &mut bytes).unwrap();
        assert_eq!(&bytes[..length], expected);
        assert!(
            filesystem
                .open(
                    &path,
                    polymer_vfs::OpenOptions {
                        read: true,
                        write: true,
                        truncate: false
                    }
                )
                .is_err()
        );
        assert!(
            filesystem
                .open(
                    &"/Game/plugin.toml".parse().unwrap(),
                    polymer_vfs::OpenOptions {
                        read: true,
                        write: false,
                        truncate: false
                    }
                )
                .is_err()
        );
    }
    let missing = runtime
        .handle()
        .invoke_with_storage_owned(
            "worker-test",
            &["onLaunch".to_owned()],
            vec![serde_json::json!({
                "launchPath": root.path().join("missing.iso"), "console": "TEST",
                "controllerTopology": { "mappings": [] },
            })],
            BTreeMap::new(),
        )
        .await;
    assert!(missing.is_err());
}
