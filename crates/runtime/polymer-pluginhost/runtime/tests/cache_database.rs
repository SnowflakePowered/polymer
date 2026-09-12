use super::*;

#[tokio::test(flavor = "current_thread")]
async fn cache_databases_preserve_payloads_and_isolate_connections() {
    let (root, registry, id) = capability_package();
    std::fs::write(
        root.path().join("fixture.js"),
        r#"
        import { caches } from "polymer:cache";
        import { connect } from "polymer:database";

        export async function exercise() {
          const local = await connect();
          await local.exec("CREATE TABLE marker (value TEXT); INSERT INTO marker VALUES ('local')");
          const localStatement = local.prepare("SELECT value FROM marker");
          const file = caches.fixture.open("nested/catalog.db", { read: false });
          const staging = await connect(file, { readOnly: false });
          await staging.exec("CREATE TABLE cue (rom_id INTEGER PRIMARY KEY, contents BLOB NOT NULL) WITHOUT ROWID");
          await staging.batch([["INSERT INTO cue VALUES (?, ?)", [1, [255, 70, 13, 10]]]]);
          let rolledBack = false;
          try {
            await staging.transactionAsync(async db => {
              await db.prepare("INSERT INTO cue VALUES (?, ?)").run([2, [0]]);
              throw new Error("abort fixture transaction");
            });
          } catch { rolledBack = true; }
          await staging.close();

          const catalog = await connect(file);
          const cueStatement = catalog.prepare("SELECT contents FROM cue ORDER BY rom_id");
          const independent = await connect();
          const localValue = await independent.prepare("SELECT value FROM marker").get();
          await independent.close();
          const stillLocal = await localStatement.get();

          let writeRejected = false;
          try { await catalog.prepare("INSERT INTO cue VALUES (?, ?)").run([3, [0]]); }
          catch { writeRejected = true; }
          const rows = await catalog.transactionAsync(db => db.prepare("SELECT contents FROM cue").all(), "read");
          const batched = await catalog.batch([["SELECT count(*) AS count FROM cue"]], "read");
          const iterated = [];
          for await (const row of cueStatement.iterate()) iterated.push(row);
          await local.close();
          const afterLocalClose = await cueStatement.get();
          await catalog.close();
          await catalog.close();
          let closedRejected = false;
          try { await cueStatement.get(); } catch { closedRejected = true; }
          const reopened = await connect(file);
          const persisted = await reopened.prepare("SELECT contents FROM cue").get();
          await reopened.close();
          return { rolledBack, localValue, stillLocal, writeRejected, rows, batched,
            iterated, afterLocalClose, closedRejected, persisted };
        }
        "#,
    )
    .unwrap();
    let runtime = test_runtime(registry, PluginRuntimeOptions::default());
    let result = runtime
        .handle()
        .invoke(&id, &["exercise".to_owned()], Vec::new())
        .await
        .unwrap();
    assert_eq!(
        result,
        serde_json::json!({
            "rolledBack": true,
            "localValue": { "value": "local" },
            "stillLocal": { "value": "local" },
            "writeRejected": true,
            "rows": [{ "contents": [255, 70, 13, 10] }],
            "batched": [[{ "count": 1 }]],
            "iterated": [{ "contents": [255, 70, 13, 10] }],
            "afterLocalClose": { "contents": [255, 70, 13, 10] },
            "closedRejected": true,
            "persisted": { "contents": [255, 70, 13, 10] },
        })
    );
}

#[tokio::test(flavor = "current_thread")]
async fn cache_databases_reject_invalid_sources_without_creating_files() {
    let (root, registry, id) = capability_package();
    std::fs::write(
        root.path().join("fixture.js"),
        r#"
        import { caches } from "polymer:cache";
        import { connect } from "polymer:database";
        export async function exercise() {
          const attempts = [
            () => connect("outside.db"),
            () => connect({ key: "fixture", path: "outside.db" }),
            () => connect(caches.fixture),
            () => connect(caches.fixture.open("../outside.db", { read: false })),
            () => connect(caches.fixture.open("C:\\outside.db", { read: false })),
            () => connect(caches.fixture.open("missing.db", { read: false })),
            () => connect(caches.fixture.open("invalid.db", { read: false })),
          ];
          const rejected = [];
          for (const attempt of attempts) {
            try { const db = await attempt(); await db.close(); rejected.push(false); }
            catch { rejected.push(true); }
          }
          return rejected;
        }
    "#,
    )
    .unwrap();
    let storage = tempfile::tempdir().unwrap();
    let directories = polymer_library::Directories::rooted(storage.path(), storage.path());
    let cache = directories.plugin_cache(&id).join("fixture");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join("invalid.db"), b"This is not a SQLite database").unwrap();
    let runtime = PluginRuntime::new(registry, directories, PluginRuntimeOptions::default());
    let result = runtime
        .handle()
        .invoke(&id, &["exercise".to_owned()], Vec::new())
        .await
        .unwrap();
    assert_eq!(
        result,
        serde_json::json!([true, true, true, true, true, true, true])
    );
    assert!(
        !cache.join("missing.db").exists(),
        "a read-only open must not create an empty database"
    );
}
