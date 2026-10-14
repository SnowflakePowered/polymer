use polymer_distribution::{Index, SignedArchive, TrustedComment};
use polymer_pluginhost::{
    loading::PluginRegistry,
    plugin::PluginInstance,
    runtime::{PluginRuntime, PluginRuntimeOptions},
};
use polymer_pluginmanifest::PackageSource;
use std::path::Path;

fn files(id: &str, version: &str) -> Vec<(&'static str, String)> {
    vec![
        (
            "plugin.toml",
            format!(
                r#"
                [plugin]
                id = "{id}"
                version = "{version}"
                class = "emulator"
                api = "^0.1"
                main = "plugin.js"
                [emulator]
                platforms = ["TEST"]
                [emulator.configuration]
                definitions = ["config/*.toml"]
                "#
            ),
        ),
        (
            "config/settings.toml",
            r#"
            [targets.settings]
            output = { format = "ini", path = "settings.ini" }
            [input]
            driver = "sdl3"
            device = "SDL-{index}"
            [input.mapping]
            [input.layouts]
            [platforms.TEST.ports]
            "#
            .to_owned(),
        ),
        (
            "plugin.js",
            r#"
            import { greeting } from "pkg:lib/greeting.ts";
            import assets from "pkg:assets";
            import data from "pkg:data.txt";
            export async function probe() {
                const names = [];
                let nested = "";
                for await (const entry of assets) {
                    names.push(entry.name);
                    if (entry.kind === "file") nested = await entry.text();
                }
                return { greeting: greeting(), data: await data.text(), names, nested };
            }
            "#
            .to_owned(),
        ),
        (
            "lib/greeting.ts",
            "export const greeting = (): string => \"hello\";".to_owned(),
        ),
        ("data.txt", "packaged".to_owned()),
        ("assets/inner.txt", "inner".to_owned()),
    ]
}

fn archive(id: &str, version: &str) -> Vec<u8> {
    let mut writer = asar::AsarWriter::new();
    for (path, contents) in files(id, version) {
        writer.write_file(path, contents, false).unwrap();
    }
    let mut bytes = Vec::new();
    writer.finalize(&mut bytes).unwrap();
    bytes
}

fn unpacked(root: &Path, id: &str) {
    for (path, contents) in files(id, "1.0.0") {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
}

fn sign(pair: &minisign::KeyPair, archive: &[u8], id: &str, version: &str) -> Vec<u8> {
    let comment = TrustedComment {
        id: id.to_owned(),
        version: semver::Version::parse(version).unwrap(),
    };
    let signature = minisign::sign(None, &pair.sk, archive, Some(&comment.to_string()), None)
        .unwrap()
        .into_string();
    SignedArchive::seal(archive, &signature)
}

fn index(root: &Path, entries: &[(&str, &minisign::KeyPair, &str)]) -> Index {
    for (repo, pair, rest) in entries {
        let path = root.join("repos").join(format!("{repo}.toml"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            format!(
                "repo = \"{repo}\"\nkeys = [\"{}\"]\n{rest}\n",
                pair.pk.to_base64()
            ),
        )
        .unwrap();
    }
    Index::read(root).unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn a_signed_archive_runs_from_memory_and_reads_its_own_files() {
    let official = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let index = index(
        &storage.path().join("index"),
        &[("polymer/plugins", &official, "official = true")],
    );
    let path = storage.path().join("packages/archive-test.asar");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        sign(
            &official,
            &archive("archive-test", "1.0.0"),
            "archive-test",
            "1.0.0",
        ),
    )
    .unwrap();

    let registry = PluginRegistry::new();
    let plugin = registry.load_archive(&path, &index).unwrap();
    assert!(matches!(
        plugin.module_loader().source(),
        PackageSource::Archive(_)
    ));
    std::fs::remove_file(&path).unwrap();

    let runtime = PluginRuntime::new(
        registry,
        polymer_library::Directories::rooted(storage.path(), storage.path()),
        PluginRuntimeOptions::default(),
    );
    let result = runtime
        .handle()
        .invoke("archive-test", &["probe".to_owned()], Vec::new())
        .await
        .unwrap();
    assert_eq!(
        result,
        serde_json::json!({
            "greeting": "hello",
            "data": "packaged",
            "names": ["inner.txt"],
            "nested": "inner",
        })
    );
}

#[test]
fn archives_load_only_when_a_key_registered_for_their_id_signed_them() {
    let official = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let author = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let index = index(
        &storage.path().join("index"),
        &[
            ("polymer/plugins", &official, "official = true"),
            ("author/plugins", &author, "scopes = [\"author\"]"),
        ],
    );
    let packages = storage.path().join("packages");
    std::fs::create_dir_all(packages.join("@author")).unwrap();
    let mut tampered = sign(
        &official,
        &archive("tampered", "1.0.0"),
        "tampered",
        "1.0.0",
    );
    tampered[40] ^= 1;
    for (name, bytes) in [
        ("unsigned.asar", archive("unsigned", "1.0.0")),
        ("tampered.asar", tampered),
        (
            "bare-by-author.asar",
            sign(
                &author,
                &archive("bare-by-author", "1.0.0"),
                "bare-by-author",
                "1.0.0",
            ),
        ),
        (
            "@author/by-official.asar",
            sign(
                &official,
                &archive("@author/by-official", "1.0.0"),
                "@author/by-official",
                "1.0.0",
            ),
        ),
        (
            "@author/scoped.asar",
            sign(
                &author,
                &archive("@author/scoped", "1.0.0"),
                "@author/scoped",
                "1.0.0",
            ),
        ),
    ] {
        std::fs::write(packages.join(name), bytes).unwrap();
    }

    let registry = PluginRegistry::new();
    let skipped = registry.discover(&[packages], &index);
    assert_eq!(skipped.len(), 4);
    let ids = registry
        .plugins::<dyn PluginInstance>()
        .iter()
        .map(|plugin| plugin.id().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(ids, ["@author/scoped"]);
}

#[test]
fn an_unpacked_plugin_takes_the_place_of_an_archive_with_the_same_id() {
    let official = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let index = index(
        &storage.path().join("index"),
        &[("polymer/plugins", &official, "official = true")],
    );
    let packages = storage.path().join("packages");
    std::fs::create_dir_all(&packages).unwrap();
    std::fs::write(
        packages.join("clash.asar"),
        sign(&official, &archive("clash", "1.0.0"), "clash", "1.0.0"),
    )
    .unwrap();
    let working = tempfile::tempdir().unwrap();
    unpacked(&working.path().join("plugins/clash"), "clash");

    let registry = PluginRegistry::new();
    let skipped = registry.discover(&[packages.clone(), working.path().join("plugins")], &index);
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].0, packages.join("clash.asar"));
    let plugin = registry.plugin::<dyn PluginInstance>("clash").unwrap();
    assert!(matches!(
        plugin.module_loader().source(),
        PackageSource::Directory(_)
    ));
}

#[test]
fn the_newest_of_several_archives_of_a_plugin_loads() {
    let official = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let index_root = storage.path().join("index");
    std::fs::create_dir_all(index_root.join("repos")).unwrap();
    std::fs::write(index_root.join("repos/README.md"), "not an entry").unwrap();
    let index = index(
        &index_root,
        &[("polymer/plugins", &official, "official = true")],
    );
    let packages = storage.path().join("packages");
    std::fs::create_dir_all(&packages).unwrap();
    for version in ["1.0.0", "2.0.0", "1.5.0"] {
        std::fs::write(
            packages.join(format!("versioned-{version}.asar")),
            sign(
                &official,
                &archive("versioned", version),
                "versioned",
                version,
            ),
        )
        .unwrap();
    }

    let registry = PluginRegistry::new();
    assert_eq!(registry.discover(&[packages], &index).len(), 2);
    let plugin = registry.plugin::<dyn PluginInstance>("versioned").unwrap();
    assert_eq!(plugin.version(), "2.0.0");
}
