use polymer_library::{Directories, StorageKind, StorageManager};
use polymer_pluginhost::{
    loading::PluginRegistry,
    platform::Device,
    runtime::{PluginRuntime, PluginRuntimeOptions},
};

use serde_json::{Value, json};
use std::time::Duration;

struct Fixture {
    root: tempfile::TempDir,
    registry: PluginRegistry,
    directories: Directories,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        Self {
            directories: Directories::rooted(root.path(), root.path()),
            root,
            registry: PluginRegistry::new(),
        }
    }
    fn provider(&self, id: &str, code: &str) -> std::path::PathBuf {
        let path = self.root.path().join(id);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(
            path.join("plugin.toml"),
            format!(
                r#"
            [plugin]
            id = "{id}"
            version = "1"
            class = "platform"
            main = "main.js"
            [platform]
            platforms = ["NINTENDO_WII"]
        "#
            ),
        )
        .unwrap();
        std::fs::write(path.join("main.js"), code).unwrap();
        path
    }
    fn emulator(&self) {
        let path = self.root.path().join("emulator");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(
            path.join("plugin.toml"),
            r#"
            [plugin]
            id = "emulator"
            version = "1"
            class = "emulator"
            main = "main.js"
            [emulator]
            platforms = ["NINTENDO_WII"]
            [emulator.configuration]
            definitions = ["config.toml"]
        "#,
        )
        .unwrap();
        std::fs::write(
            path.join("config.toml"),
            r#"
            [targets.settings]
            output = { format = "ini", path = "settings.ini" }
            [input]
            driver = "sdl3"
            device = "SDL-{index}"
            [input.mapping]
            [input.layouts]
            [platforms.NINTENDO_WII.ports]
        "#,
        )
        .unwrap();
        std::fs::write(
            path.join("main.js"),
            "export function onLaunch() { throw new Error('ON_LAUNCH_REACHED'); }",
        )
        .unwrap();
        self.registry.load(path).unwrap();
    }
    fn runtime(&self) -> PluginRuntime {
        PluginRuntime::new(
            self.registry.clone(),
            self.directories.clone(),
            PluginRuntimeOptions::default(),
        )
    }
    async fn device(&self) -> Device {
        let volume = StorageManager::new(self.directories.consolestorage())
            .create(StorageKind::Nand, "NINTENDO_WII", None)
            .await
            .unwrap();
        Device {
            platform: "NINTENDO_WII".into(),
            id: volume.metadata().await.unwrap().id,
        }
    }
}

#[tokio::test]
async fn duplicate_provider_and_direct_bootstrap_are_rejected() {
    let fixture = Fixture::new();
    fixture
        .registry
        .load(fixture.provider("first", "export function onBootstrap() {}"))
        .unwrap();
    let duplicate = fixture.registry.load(fixture.provider("second", ""));
    assert!(
        duplicate
            .err()
            .unwrap()
            .to_string()
            .contains("duplicate providers")
    );
    assert_eq!(
        fixture.registry.platform("NINTENDO_WII").unwrap().id(),
        "first"
    );
    let runtime = fixture.runtime();
    let error = runtime
        .handle()
        .invoke("first", &["onBootstrap".into()], vec![])
        .await
        .unwrap_err();
    assert_eq!(error.code, "BOOTSTRAP_REQUIRES_DEVICE");
}

#[tokio::test]
async fn launch_does_not_gate_on_nand_readiness() {
    let fixture = Fixture::new();
    fixture.emulator();
    let runtime = fixture.runtime();
    let device = fixture.device().await;
    let manager = StorageManager::new(fixture.directories.consolestorage());
    device
        .open(&manager)
        .await
        .unwrap()
        .root()
        .create("tmp/partial")
        .unwrap()
        .write_all_at(0, b"unfinished")
        .unwrap();
    let arguments = vec![json!({
        "console": "NINTENDO_WII",
        "controllerTopology": {"mappings": []},
        "storage": {"nand": ["00000000000000000000000000000000", device.id]},
    })];
    for selection in [
        json!(["00000000000000000000000000000000", device.id]),
        json!([device.id, "00000000000000000000000000000000"]),
        json!(device.id),
    ] {
        let mut selected = arguments.clone();
        selected[0]["storage"]["nand"] = selection;
        let error = runtime
            .handle()
            .invoke("emulator", &["onLaunch".into()], selected)
            .await
            .unwrap_err();
        assert!(error.message.contains("ON_LAUNCH_REACHED"), "{error}");
    }
    let nand = device.open(&manager).await.unwrap();
    assert!(nand.root().exists("tmp/partial"));
    nand.recover().unwrap();
    let error = runtime
        .handle()
        .invoke("emulator", &["onLaunch".into()], arguments)
        .await
        .unwrap_err();
    assert!(error.message.contains("ON_LAUNCH_REACHED"), "{error}");
}

#[tokio::test]
async fn cancelling_bootstrap_terminates_the_worker() {
    let fixture = Fixture::new();
    fixture
        .registry
        .load(fixture.provider(
            "platform",
            r#"
        import { pluginStorage } from 'polymer:pluginStorage';
        export async function onBootstrap() {
            pluginStorage.setItem('entered', true);
            await new Promise(resolve => setTimeout(resolve, 60000));
            return {issues: []};
        }
        export function entered() { return pluginStorage.getItem('entered') === true; }
    "#,
        ))
        .unwrap();
    let runtime = fixture.runtime();
    let device = fixture.device().await;
    let token = runtime.handle().cancellation_token();
    let handle = runtime.handle().clone();
    let cancellation = token.clone();
    let operation =
        tokio::spawn(async move { handle.bootstrap(device, Value::Null, cancellation).await });
    tokio::time::timeout(Duration::from_secs(10), async {
        while runtime
            .handle()
            .invoke("platform", &["entered".into()], vec![])
            .await
            .unwrap()
            != true
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    token.cancel();
    let error = tokio::time::timeout(Duration::from_secs(5), operation)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, "PLUGIN_CANCELLED");
}

#[tokio::test]
async fn bootstrap_uses_existing_progress_events_for_callbacks_and_generators() {
    use polymer_pluginhost::runtime::PluginEvent;
    let fixture = Fixture::new();
    fixture
        .registry
        .load(fixture.provider(
            "progress-platform",
            r#"

        export async function* onBootstrap(device, context) {
            context.progress({message: 'Callback progress', completed: 1, total: 2});
            yield {message: 'Generator progress', completed: 2, total: 2};
            throw new Error('BOOTSTRAP_FAILURE');
        }
    "#,
        ))
        .unwrap();
    let runtime = fixture.runtime();
    let device = fixture.device().await;
    let mut events = PluginEvent::subscribe();
    let error = runtime
        .handle()
        .bootstrap(device, Value::Null, runtime.handle().cancellation_token())
        .await
        .unwrap_err();
    assert!(error.message.contains("BOOTSTRAP_FAILURE"));
    let mut messages = Vec::new();
    while let Ok(event) = events.try_recv() {
        if event.plugin_id == "progress-platform" && event.name == "loadprogress" {
            messages.push(event.detail["message"].as_str().unwrap().to_owned());
        }
    }
    let callback = messages
        .iter()
        .position(|message| message == "Callback progress")
        .unwrap();
    let generator = messages
        .iter()
        .position(|message| message == "Generator progress")
        .unwrap();
    assert!(callback < generator);
}

impl Fixture {
    fn title_source(&self, root: &std::path::Path) {
        for directory in ["code", "content", "meta"] {
            std::fs::create_dir_all(root.join(directory)).unwrap();
        }
        std::fs::write(
            root.join("code/app.xml"),
            "<app><title_id>0005000c12345678</title_id></app>",
        )
        .unwrap();
        std::fs::write(root.join("code/cos.xml"), "<app/>").unwrap();
        std::fs::write(
            root.join("meta/meta.xml"),
            "<menu><title_id>0005000c12345678</title_id><title_version>7</title_version></menu>",
        )
        .unwrap();
        std::fs::write(root.join("content/data.bin"), b"content").unwrap();
        let ticket = polymer_consolestorage::impls::wiiu::ticket::WiiUTicket::synthesize(
            0x0005000c12345678,
            7,
            &[0x42; 16],
        )
        .unwrap();
        std::fs::write(root.join("code/title.tik"), ticket).unwrap();
    }
}

#[tokio::test]
async fn platform_install_persists_keys_before_validation_and_installs_content() {
    use polymer_consolestorage::keys::{KeyIdentity, KeyReader};
    use polymer_library::keys::KeyStore;
    use polymer_pluginhost::platform::DepotSource;
    let fixture = Fixture::new();
    let path = fixture.root.path().join("title");
    fixture.title_source(&path);
    let source: DepotSource = serde_json::from_value(json!({"path": path})).unwrap();
    let manager = StorageManager::new(fixture.directories.consolestorage());
    let volume = manager
        .create(StorageKind::Nand, "NINTENDO_WIIU", None)
        .await
        .unwrap();
    let device = Device {
        platform: "NINTENDO_WIIU".into(),
        id: volume.metadata().await.unwrap().id,
    };
    let nand = device.open(&manager).await.unwrap();
    nand.root()
        .create("unfinished")
        .unwrap()
        .write_all_at(0, b"dirty")
        .unwrap();
    let error = source
        .install(&device, &manager, &path, |_| {})
        .await
        .unwrap_err();
    assert!(error.contains("uncommitted"), "{error}");
    let store = KeyStore::open(fixture.directories.keys()).await.unwrap();
    let keys = store.for_platform("NINTENDO_WIIU").unwrap();
    let key = keys
        .read(&KeyIdentity {
            namespace: "content".into(),
            id: "0005000c12345678".into(),
        })
        .await
        .unwrap();
    assert_eq!(key.bytes, [0x42; 16]);
    nand.recover().unwrap();
    let installed = source
        .install(&device, &manager, &path, |_| {})
        .await
        .unwrap();
    assert_eq!(installed.id, "0005000c12345678");
    let composed = nand.compose(None, &keys).await.unwrap();
    let root = "usr/title/0005000c/12345678";
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(
        &mut composed
            .open_read(&format!("{root}/code/title.tik"))
            .unwrap(),
        &mut bytes,
    )
    .unwrap();
    assert_eq!(
        polymer_consolestorage::impls::wiiu::ticket::WiiUTicket::decode(&bytes)
            .unwrap()
            .bytes,
        key.bytes
    );
    assert_eq!(&bytes[0x1e6..0x1e8], &7u16.to_be_bytes());
    assert!(composed.exists(&format!("{root}/content/data.bin")));
    assert!(!nand.is_dirty().unwrap());

    // A recognized source with a malformed ticket is a preparation error.
    std::fs::write(path.join("code/title.tik"), b"truncated").unwrap();
    assert!(nand.try_install(&path).is_err());
    std::fs::remove_file(path.join("code/title.tik")).unwrap();
    assert!(nand.try_install(&path).unwrap().embedded_keys().is_empty());
    source
        .install(&device, &manager, &path, |_| {})
        .await
        .unwrap();
}

#[tokio::test]
async fn bootstrap_uses_native_requirements_with_one_worker() {
    use polymer_consolestorage::keys::{KeyIdentity, KeyReader};
    let fixture = Fixture::new();
    let package = fixture.provider(
        "kind-check",
        r#"
        export async function onBootstrap(device) { await device.install('pkg:title'); }
    "#,
    );
    let manifest = package.join("plugin.toml");
    std::fs::write(
        &manifest,
        std::fs::read_to_string(&manifest)
            .unwrap()
            .replace("NINTENDO_WII", "NINTENDO_WIIU"),
    )
    .unwrap();
    fixture.title_source(&package.join("title"));
    fixture.registry.load(package).unwrap();
    let manager = StorageManager::new(fixture.directories.consolestorage());
    let volume = manager
        .create(StorageKind::Nand, "NINTENDO_WIIU", None)
        .await
        .unwrap();
    let device = Device {
        platform: "NINTENDO_WIIU".into(),
        id: volume.metadata().await.unwrap().id,
    };
    let nand = device.open(&manager).await.unwrap();
    nand.root()
        .create("unfinished")
        .unwrap()
        .write_all_at(0, b"dirty")
        .unwrap();
    let runtime = PluginRuntime::new(
        fixture.registry.clone(),
        fixture.directories.clone(),
        PluginRuntimeOptions {
            max_concurrent_calls: 1,
            ..Default::default()
        },
    );
    let error = tokio::time::timeout(
        Duration::from_secs(10),
        runtime
            .handle()
            .bootstrap(device, Value::Null, runtime.handle().cancellation_token()),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.message.contains("uncommitted"), "{error}");
    let store = polymer_library::keys::KeyStore::open(fixture.directories.keys())
        .await
        .unwrap();
    let record = store
        .for_platform("NINTENDO_WIIU")
        .unwrap()
        .read(&KeyIdentity {
            namespace: "content".into(),
            id: "0005000c12345678".into(),
        })
        .await
        .unwrap();
    assert_eq!(record.bytes, [0x42; 16]);
}

#[tokio::test]
async fn try_install_retains_depot_without_starting_extraction() {
    let fixture = Fixture::new();
    let path = fixture.root.path().join("title");
    fixture.title_source(&path);
    let manager = StorageManager::new(fixture.directories.consolestorage());
    let volume = manager
        .create(StorageKind::Nand, "NINTENDO_WIIU", None)
        .await
        .unwrap();
    let device = Device {
        platform: "NINTENDO_WIIU".into(),
        id: volume.metadata().await.unwrap().id,
    };
    let nand = device.open(&manager).await.unwrap();
    let installation = nand.try_install(&path).unwrap();
    assert!(!nand.is_dirty().unwrap());
    assert!(nand.artifacts().await.unwrap().is_empty());
    let artifact = installation.finish().await.unwrap();
    assert_eq!(artifact.id(), "0005000c12345678");
}

#[tokio::test]
async fn rust_and_javascript_share_platform_keys_and_immutable_files() {
    use polymer_consolestorage::keys::{KeyIdentity, KeyQuery, KeyReader, KeyRecord, KeyWriter};
    use polymer_library::keys::KeyStore;
    let fixture = Fixture::new();
    let store = KeyStore::open(fixture.directories.keys()).await.unwrap();
    let keys = store.for_platform("NINTENDO_WII").unwrap();
    let identity = KeyIdentity {
        namespace: "content".into(),
        id: "license".into(),
    };
    keys.put(&[KeyRecord {
        identity: identity.clone(),
        title_id: Some("title".into()),
        bytes: vec![1, 2, 3],
    }])
    .await
    .unwrap();
    fixture.registry.load(fixture.provider("key-platform", r#"
        export async function onBootstrap(device, {keyStore}) {
            const entries = await keyStore.list({namespace: 'content', titleId: 'title'});
            if (entries.length !== 1 || entries[0].byteLength !== 3) throw new Error('bad enumeration');
            const opened = await keyStore.open(entries[0]);
            await keyStore.put([{namespace: 'content', id: 'license', bytes: new Uint8Array([4, 5])}]);
            if ((await keyStore.list({titleId: 'title'})).length !== 0) throw new Error('stale title association');
            if (String(await (await keyStore.open(entries[0])).bytes()) !== '4,5') throw new Error('replacement not visible');
            if (!await keyStore.remove(entries[0])) throw new Error('remove failed');
            if (String(await opened.bytes()) !== '1,2,3') throw new Error('open resource changed');
            await keyStore.put([{namespace: 'system', id: 'native_name', bytes: new Uint8Array([7])}]);
        }
    "#)).unwrap();
    let runtime = fixture.runtime();
    runtime
        .handle()
        .bootstrap(
            fixture.device().await,
            Value::Null,
            runtime.handle().cancellation_token(),
        )
        .await
        .unwrap();
    assert!(keys.read(&identity).await.is_err());
    assert_eq!(
        keys.read(&KeyIdentity {
            namespace: "system".into(),
            id: "native_name".into()
        })
        .await
        .unwrap()
        .bytes,
        [7]
    );
    assert!(
        store
            .for_platform("SONY_PS3")
            .unwrap()
            .list(&KeyQuery::default())
            .await
            .unwrap()
            .is_empty()
    );

    fixture.emulator();
    std::fs::write(fixture.root.path().join("emulator/main.js"), r#"
        export async function onLaunch({keyStore, filesystem}) {
            const entry = await keyStore.open({namespace: 'system', id: 'native_name'});
            if (String(await entry.bytes()) !== '7') throw new Error('Rust/JS read failed');
            let denied = false;
            try { await __polymerKeys.writer.put([{namespace: 'system', id: 'native_name', bytes: new Uint8Array([9])}]); }
            catch (error) { denied = String(error).includes('read-only'); }
            if (!denied) throw new Error('host did not enforce reader');
            denied = false;
            try { filesystem.mountFile('/key', entry, {mode: 'rw'}); }
            catch { denied = true; }
            if (!denied) throw new Error('key resource permitted writable mount');
            throw new Error('KEY_READER_VERIFIED');
        }
    "#).unwrap();
    let error = runtime
        .handle()
        .invoke(
            "emulator",
            &["onLaunch".into()],
            vec![json!({
                "console": "NINTENDO_WII", "controllerTopology": {"mappings": []},
            })],
        )
        .await
        .unwrap_err();
    assert!(error.message.contains("KEY_READER_VERIFIED"), "{error}");
}
