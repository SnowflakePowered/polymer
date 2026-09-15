use polymer_library::{Directories, StorageKind, StorageManager};
use polymer_pluginhost::{
    loading::PluginRegistry,
    platform::{Device, Operation},
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
            [emulator.preflight.NINTENDO_WII.nand]
            completion = "minimal"
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
    fixture.registry.load(fixture.provider("first", "export function onBootstrap() {} export function onPreflight() { return {issues: []}; }")).unwrap();
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
async fn missing_nand_blocks_all_launch_export_forms_before_on_launch() {
    let fixture = Fixture::new();
    fixture.emulator();
    let runtime = fixture.runtime();
    for path in [
        vec!["onLaunch".into()],
        vec!["default".into(), "onLaunch".into()],
    ] {
        let error = runtime
            .handle()
            .invoke("emulator", &path, vec![json!({"console":"NINTENDO_WII"})])
            .await
            .unwrap_err();
        assert_eq!(error.code, "PREFLIGHT_BLOCKED");
        assert_eq!(
            error.details.unwrap()["issues"][0]["code"],
            "DEVICE_REQUIRED"
        );
    }
}

#[tokio::test]
async fn dirty_nand_blocks_on_launch_until_explicit_recovery() {
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
    let lease = polymer_vfs::ExclusiveResourceKey::Resource(device.id)
        .acquire()
        .unwrap();
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
        assert_eq!(error.code, "PREFLIGHT_BLOCKED");
        assert_eq!(error.details.unwrap()["issues"][0]["code"], "NAND_DIRTY");
    }
    drop(lease);
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
async fn preflight_has_no_network_even_when_provider_declares_hosts() {
    let fixture = Fixture::new();
    let package = fixture.provider("platform", r#"
        export async function onPreflight() {
            try { await fetch('http://127.0.0.1:41080/'); }
            catch (error) { return {issues:[{code:'NETWORK_BLOCKED', message: String(error), severity:'blocking'}]}; }
            throw new Error('network was available');
        }
    "#);
    use std::io::Write;
    writeln!(
        std::fs::OpenOptions::new()
            .append(true)
            .open(package.join("plugin.toml"))
            .unwrap(),
        "\n[plugin.permissions.net]\nallowed-hosts = [\"127.0.0.1\"]"
    )
    .unwrap();
    fixture.registry.load(package).unwrap();
    let runtime = fixture.runtime();
    let device = fixture.device().await;
    let report = runtime
        .handle()
        .preflight(
            device
                .request(
                    &StorageManager::new(fixture.directories.consolestorage()),
                    Operation::Bootstrap,
                    Value::Null,
                    None,
                )
                .await
                .unwrap(),
        )
        .await
        .unwrap();
    let message = report.issues[0].message.to_lowercase();
    assert!(
        message.contains("notcapable")
            || message.contains("permission")
            || message.contains("not allowed")
            || message.contains("denied"),
        "{message}"
    );
}

#[tokio::test]
async fn cancelling_bootstrap_preflight_terminates_the_worker() {
    let fixture = Fixture::new();
    fixture
        .registry
        .load(fixture.provider(
            "platform",
            r#"
        import { pluginStorage } from 'polymer:pluginStorage';
        export async function onPreflight() {
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
async fn platform_provider_is_optional_for_native_preflight() {
    let fixture = Fixture::new();
    let runtime = fixture.runtime();
    let device = fixture.device().await;
    let report = runtime
        .handle()
        .preflight(
            device
                .request(
                    &StorageManager::new(fixture.directories.consolestorage()),
                    Operation::Install,
                    Value::Null,
                    None,
                )
                .await
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(report.issues.is_empty());
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
        export function onPreflight() { return {issues: []}; }
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

#[tokio::test]
async fn platform_constructors_find_and_install_a_rap_without_a_format_hint() {
    use polymer_pluginhost::platform::DepotSource;
    use std::cell::RefCell;
    let fixture = Fixture::new();
    let content = "UP0001-NPUB12345_00-0123456789ABCDEF";
    // Recognition comes from the constructor, not a filename extension selector.
    let path = fixture.root.path().join(format!("{content}.bin"));
    std::fs::write(&path, [0x42; 16]).unwrap();
    let source: DepotSource = serde_json::from_value(json!({"path": path})).unwrap();
    let manager = StorageManager::new(fixture.directories.consolestorage());
    let volume = manager
        .create(StorageKind::Nand, "SONY_PS3", None)
        .await
        .unwrap();
    let device = Device {
        platform: "SONY_PS3".into(),
        id: volume.metadata().await.unwrap().id,
    };
    let nand = device.open(&manager).await.unwrap();
    assert!(nand.artifacts().await.unwrap().is_empty());
    let wii = fixture.device().await;
    assert!(
        source
            .install(&wii, &manager, &path, |_| panic!(
                "incompatible NAND must not install"
            ))
            .await
            .is_err()
    );
    let progress = RefCell::new(Vec::new());
    let installed = source
        .install(&device, &manager, &path, |item| {
            progress.borrow_mut().push(item)
        })
        .await
        .unwrap();
    assert_eq!(installed["id"], format!("{content}:rap"));
    let target = format!("dev_hdd0/home/00000001/exdata/{content}.rap");
    assert_eq!(*progress.borrow(), vec![target.clone()]);
    let nand = device.open(&manager).await.unwrap();
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut nand.root().open_read(&target).unwrap(), &mut bytes).unwrap();
    assert_eq!(bytes, vec![0x42; 16]);
    std::fs::write(&path, [0x42; 15]).unwrap();
    assert!(
        source
            .install(&device, &manager, &path, |_| {})
            .await
            .is_err()
    );
}

#[tokio::test]
async fn bootstrap_runs_platform_preflight_separately_with_one_worker() {
    let fixture = Fixture::new();
    let content = "UP0001-NPUB12345_00-0123456789ABCDEF";
    let package = fixture.provider("kind-check", &format!(r#"
        export function onPreflight(request) {{
            if (request.operation !== 'install') return {{issues: []}};
            if ('depot' in request || request.depot_kind !== 'appticket') throw new Error('wrong preflight contract');
            return {{issues:[{{code:'KIND_BLOCKED',message:'license installation blocked',severity:'blocking'}}]}};
        }}
        export async function onBootstrap(device) {{ await device.install('pkg:{content}.rap'); }}
    "#));
    let manifest = package.join("plugin.toml");
    std::fs::write(
        &manifest,
        std::fs::read_to_string(&manifest)
            .unwrap()
            .replace("NINTENDO_WII", "SONY_PS3"),
    )
    .unwrap();
    std::fs::write(package.join(format!("{content}.rap")), [0x42; 16]).unwrap();
    fixture.registry.load(package).unwrap();
    let manager = StorageManager::new(fixture.directories.consolestorage());
    let volume = manager
        .create(StorageKind::Nand, "SONY_PS3", None)
        .await
        .unwrap();
    let device = Device {
        platform: "SONY_PS3".into(),
        id: volume.metadata().await.unwrap().id,
    };
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
        runtime.handle().bootstrap(
            device.clone(),
            Value::Null,
            runtime.handle().cancellation_token(),
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(
        error.message.contains("license installation blocked"),
        "{error}"
    );
    assert!(
        device
            .open(&manager)
            .await
            .unwrap()
            .artifacts()
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn try_install_retains_depot_without_starting_extraction() {
    let fixture = Fixture::new();
    let manager = StorageManager::new(fixture.directories.consolestorage());
    let volume = manager
        .create(StorageKind::Nand, "SONY_PS3", None)
        .await
        .unwrap();
    let device = Device {
        platform: "SONY_PS3".into(),
        id: volume.metadata().await.unwrap().id,
    };
    let nand = device.open(&manager).await.unwrap();
    let content = "UP0001-NPUB12345_00-0123456789ABCDEF";
    let path = fixture.root.path().join(format!("{content}.rap"));
    std::fs::write(&path, [0x42; 16]).unwrap();
    let installation = nand.try_install(&path).unwrap();
    assert!(nand.artifacts().await.unwrap().is_empty());
    std::fs::remove_file(&path).unwrap();
    let artifact = installation.finish().await.unwrap();
    assert_eq!(artifact.id(), format!("{content}:rap"));
}
