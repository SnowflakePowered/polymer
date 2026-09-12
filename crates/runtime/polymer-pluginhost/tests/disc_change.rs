use polymer_pluginhost::{
    controller::{DiscChange, Quit},
    loading::PluginRegistry,
    runtime::{PluginRuntime, PluginRuntimeOptions},
};
use std::{collections::BTreeMap, time::Duration};

#[tokio::test(flavor = "current_thread")]
async fn disc_changes_keep_quit_on_the_last_successful_disc() {
    for export in [
        vec!["onLaunch".to_owned()],
        vec!["default".to_owned(), "onLaunch".to_owned()],
    ] {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("plugin.toml"),
            r#"
            [plugin]
            id = "worker-test"
            version = "1"
            class = "emulator"
            main = "plugin.js"
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
            let current = 1;
            let busy = false;
            const plugin = {
              onLaunch(args) {
                if (args.initialDiscIndex !== 1 || args.launchPath !== "disc1") throw Error("wrong initial disc");
                if (!Object.isFrozen(args.launchSet) || !Object.isFrozen(args.launchSet[0])) throw Error("mutable set");
                return { command: "emulator.exe", hooks: { graphics: ["vulkan"], input: [] }, surfaces: [{ index: 0 }] };
              },
              onParseHotkey(code) { return { kind: "keyboard", code }; },
              async onDiscChange(context) {
                if (this !== plugin || busy) throw Error("callback receiver or serialization lost");
                if (context.index === 2) throw Error("disc unavailable");
                if (!context.filesystem || context.launchSet[context.index].launchPath !== `disc${context.index}`) throw Error("wrong context");
                busy = true;
                await new Promise(resolve => setTimeout(resolve, 5));
                current = context.index;
                busy = false;
              },
              onQuit(context) {
                if (context.launchPath !== `disc${current}` || context.package.serial !== `${current}`) throw Error("quit disc mismatch");
                return null;
              },
            };
            export default plugin;
        "#).unwrap();
        let registry = PluginRegistry::new();
        registry.load(root.path()).unwrap();
        let storage = tempfile::tempdir().unwrap();
        let directories = polymer_library::Directories::rooted(storage.path(), storage.path());
        let runtime = PluginRuntime::new(registry, directories, PluginRuntimeOptions::default());
        let result = runtime
            .handle()
            .invoke_with_storage_owned(
                "worker-test",
                &export,
                vec![serde_json::json!({
                    "console": "TEST", "controllerTopology": { "mappings": [] },
                    "launchSet": [
                        { "launchPath": "disc0", "package": { "serial": "0" } },
                        { "launchPath": "disc1", "package": { "serial": "1" } },
                        { "launchPath": "disc2", "package": { "serial": "2" } },
                    ],
                    "initialDiscIndex": 1,
                })],
                BTreeMap::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.value["canChangeDisc"], true);
        assert_eq!(result.value["initialDiscIndex"], 1);
        let session = result.controller_session.unwrap();
        let timeout = Duration::from_secs(2);
        assert!(
            session
                .send(Quit { timeout })
                .await
                .unwrap()
                .unwrap()
                .is_none()
        );
        let error = session
            .send(DiscChange { index: 2, timeout })
            .await
            .unwrap()
            .unwrap_err();
        assert!(error.contains("disc unavailable"));
        session.send(Quit { timeout }).await.unwrap().unwrap();
        assert!(
            session
                .send(DiscChange { index: 3, timeout })
                .await
                .unwrap()
                .is_err()
        );
        let first = session.send(DiscChange { index: 0, timeout });
        let second = session.send(DiscChange { index: 1, timeout });
        let (first, second) = tokio::join!(first, second);
        first.unwrap().unwrap();
        second.unwrap().unwrap();
        session.send(Quit { timeout }).await.unwrap().unwrap();
        session.shutdown().await.unwrap();
    }
}
