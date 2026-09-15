use super::*;
use polymer_deno::polymer::consolestorage::*;
use polymer_library::StorageKind;
use polymer_pluginhost::{
    loading::PluginRegistry,
    platform::Device,
    runtime::{PluginRuntime, PluginRuntimeOptions},
};
use polymer_vfs::ExclusiveResourceKey;
use serde_json::json;
use std::time::Duration;
use tokio::{sync::Notify, time::timeout};

struct Fixture {
    root: tempfile::TempDir,
    manager: Arc<StorageManager>,
    runtime: PluginRuntime,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let directories = Directories::rooted(root.path(), root.path());
        let manager = Arc::new(StorageManager::new(directories.consolestorage()));
        let runtime = PluginRuntime::new(
            PluginRegistry::new(),
            directories,
            PluginRuntimeOptions::default(),
        );
        Self {
            root,
            manager,
            runtime,
        }
    }

    async fn device(&self, platform: &str) -> Device {
        let storage = self
            .manager
            .create(StorageKind::Nand, platform, None)
            .await
            .unwrap();
        Device {
            platform: platform.into(),
            id: storage.metadata().await.unwrap().id,
        }
    }
}

#[tokio::test]
async fn bootstrap_keeps_its_lease_and_actor_alive_without_blocking_other_requests() {
    let fixture = Fixture::new();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let router = axum::Router::new().route(
        "/",
        axum::routing::get({
            let entered = entered.clone();
            let release = release.clone();
            move || {
                let entered = entered.clone();
                let release = release.clone();
                async move {
                    entered.notify_one();
                    release.notified().await;
                    "system files"
                }
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let package = fixture.root.path().join("platform");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("plugin.toml"),
        r#"
        [plugin]
        id = "storage-actor-test"
        version = "1"
        class = "platform"
        main = "main.js"
        [platform]
        platforms = ["NINTENDO_WII"]
        [plugin.permissions.net]
        allowed-hosts = ["127.0.0.1"]
    "#,
    )
    .unwrap();
    std::fs::write(
        package.join("main.js"),
        format!(
            r#"
        export async function onBootstrap() {{
            const response = await fetch('http://{address}/');
            await response.text();
        }}
    "#
        ),
    )
    .unwrap();
    fixture.runtime.handle().registry().load(package).unwrap();
    let device = fixture.device("NINTENDO_WII").await;
    let other = fixture.device("NINTENDO_WII").await;
    let service =
        StorageService::start(Some(fixture.manager.clone()), fixture.runtime.handle()).unwrap();
    let client = service.client();
    let mut events = polymer_pluginhost::runtime::PluginEvent::subscribe();
    let mut bootstrap = Box::pin(client.bootstrap.send(Bootstrap {
        platform: device.platform.clone(),
        id: device.id.to_string(),
        settings: json!({}),
    }));
    timeout(Duration::from_secs(15), async {
        tokio::select! {
            _ = entered.notified() => {},
            result = &mut bootstrap => panic!("bootstrap ended before download: {result:?}"),
        }
    })
    .await
    .unwrap();

    // Closing the caller must not abandon an installation or release its lease.
    drop(bootstrap);
    let listed = timeout(Duration::from_secs(3), client.list.send(List))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 2);
    timeout(
        Duration::from_secs(3),
        client.inspect.send(Inspect {
            platform: other.platform,
            id: other.id.to_string(),
        }),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    let errors = timeout(Duration::from_secs(3), async {
        vec![
            client
                .inspect
                .send(Inspect {
                    platform: device.platform.clone(),
                    id: device.id.to_string(),
                })
                .await
                .unwrap()
                .unwrap_err(),
            client
                .delete
                .send(Delete {
                    kind: "nand".into(),
                    platform: device.platform.clone(),
                    id: device.id.to_string(),
                })
                .await
                .unwrap()
                .unwrap_err(),
            client
                .set_label
                .send(SetLabel {
                    kind: "nand".into(),
                    platform: device.platform.clone(),
                    id: device.id.to_string(),
                    label: Some("busy".into()),
                })
                .await
                .unwrap()
                .unwrap_err(),
        ]
    })
    .await
    .unwrap();
    for error in errors {
        assert!(error.contains("overlaps the active resource"), "{error}");
    }
    drop(client);
    let actor = service.actor.downgrade();
    drop(service);
    assert!(actor.upgrade().is_some());
    assert!(ExclusiveResourceKey::Resource(device.id).acquire().is_err());
    release.notify_one();
    timeout(Duration::from_secs(10), async {
        loop {
            let event = events.recv().await.unwrap();
            if event.plugin_id == "storage-actor-test"
                && event.name == "loadprogress"
                && event.detail["state"] == "ready"
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    timeout(Duration::from_secs(10), async {
        while actor.upgrade().is_some() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(ExclusiveResourceKey::Resource(device.id).acquire().is_ok());
    server.abort();
}

#[tokio::test]
async fn actor_install_and_removal_preserve_artifacts_and_reject_stale_plans() {
    let fixture = Fixture::new();
    let device = fixture.device("SONY_PS3").await;
    let nand = device.open(&fixture.manager).await.unwrap();
    nand.root()
        .create("dev_flash/sys/external/liblv2.sprx")
        .unwrap()
        .write_all_at(0, b"firmware")
        .unwrap();
    nand.set_label(Some("Initialized test NAND")).await.unwrap();
    let service =
        StorageService::start(Some(fixture.manager.clone()), fixture.runtime.handle()).unwrap();
    let client = service.client();
    let mut artifacts = Vec::new();
    let mut first_plan = Value::Null;
    for title in ["NPUB12345", "NPUB54321"] {
        let content = format!("UP0001-{title}_00-0123456789ABCDEF");
        let path = fixture.root.path().join(format!("{content}.rap"));
        std::fs::write(&path, [0x42; 16]).unwrap();
        let installed = client
            .install
            .send(Install {
                platform: device.platform.clone(),
                id: device.id.to_string(),
                source: json!({"path": path}),
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(installed["id"], format!("{content}:rap"));
        artifacts.push(installed["id"].as_str().unwrap().to_owned());
        if first_plan.is_null() {
            first_plan = client
                .plan_removal
                .send(PlanRemoval {
                    platform: device.platform.clone(),
                    id: device.id.to_string(),
                    ids: vec![artifacts[0].clone()],
                })
                .await
                .unwrap()
                .unwrap();
        }
    }
    let error = client
        .uninstall
        .send(Uninstall {
            platform: device.platform.clone(),
            id: device.id.to_string(),
            ids: vec![artifacts[0].clone()],
            revision: first_plan["revision"].as_str().unwrap().to_owned(),
        })
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.contains("NAND contents changed"), "{error}");
    let before = client
        .inspect
        .send(Inspect {
            platform: device.platform.clone(),
            id: device.id.to_string(),
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(before["artifacts"].as_array().unwrap().len(), 2);
    let plan = client
        .plan_removal
        .send(PlanRemoval {
            platform: device.platform.clone(),
            id: device.id.to_string(),
            ids: vec![artifacts[1].clone()],
        })
        .await
        .unwrap()
        .unwrap();
    let remaining = client
        .uninstall
        .send(Uninstall {
            platform: device.platform.clone(),
            id: device.id.to_string(),
            ids: vec![artifacts[1].clone()],
            revision: plan["revision"].as_str().unwrap().to_owned(),
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(remaining.as_array().unwrap().len(), 1);
    assert_eq!(remaining[0]["id"], artifacts[0]);
}

#[tokio::test]
async fn dirty_nand_blocks_install_until_actor_recovery() {
    let fixture = Fixture::new();
    let device = fixture.device("SONY_PS3").await;
    let service =
        StorageService::start(Some(fixture.manager.clone()), fixture.runtime.handle()).unwrap();
    let client = service.client();
    let source = fixture
        .root
        .path()
        .join("UP0001-NPUB12345_00-0123456789ABCDEF.rap");
    std::fs::write(&source, [0x42; 16]).unwrap();
    let nand = device.open(&fixture.manager).await.unwrap();
    nand.root()
        .create("dev_flash/sys/external/liblv2.sprx")
        .unwrap()
        .write_all_at(0, b"firmware")
        .unwrap();
    nand.set_label(Some("Initialized test NAND")).await.unwrap();
    nand.root()
        .create("partial-install")
        .unwrap()
        .write_all_at(0, b"partial")
        .unwrap();
    drop(nand);
    let inspection = client
        .inspect
        .send(Inspect {
            platform: device.platform.clone(),
            id: device.id.to_string(),
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(inspection["dirty"], true);
    let error = client
        .install
        .send(Install {
            platform: device.platform.clone(),
            id: device.id.to_string(),
            source: json!({"path": source}),
        })
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.contains("uncommitted"), "{error}");
    assert!(
        device
            .open(&fixture.manager)
            .await
            .unwrap()
            .root()
            .exists("partial-install")
    );
    let lease = ExclusiveResourceKey::Resource(device.id).acquire().unwrap();
    let error = client
        .install
        .send(Install {
            platform: device.platform.clone(),
            id: device.id.to_string(),
            source: json!({"path": source}),
        })
        .await
        .unwrap()
        .unwrap_err();
    assert!(
        !error.contains("uncommitted"),
        "installation must acquire its lease first: {error}"
    );
    assert!(
        client
            .recover
            .send(Recover {
                platform: device.platform.clone(),
                id: device.id.to_string(),
            })
            .await
            .unwrap()
            .is_err()
    );
    drop(lease);
    let recovered = client
        .recover
        .send(Recover {
            platform: device.platform.clone(),
            id: device.id.to_string(),
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered["dirty"], false);
    assert!(recovered["artifacts"].as_array().unwrap().is_empty());
    assert!(
        !device
            .open(&fixture.manager)
            .await
            .unwrap()
            .root()
            .exists("partial-install")
    );
    client
        .install
        .send(Install {
            platform: device.platform.clone(),
            id: device.id.to_string(),
            source: json!({"path": source}),
        })
        .await
        .unwrap()
        .unwrap();
    let inspection = client
        .inspect
        .send(Inspect {
            platform: device.platform,
            id: device.id.to_string(),
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(inspection["dirty"], false);
    assert_eq!(inspection["artifacts"].as_array().unwrap().len(), 1);
}
