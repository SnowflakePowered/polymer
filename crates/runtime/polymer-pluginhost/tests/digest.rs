use polymer_pluginhost::{
    digest::{DigestAlgorithm, Digestible, PackageBinary},
    loading::PluginRegistry,
    runtime::{PluginRuntime, PluginRuntimeOptions},
};

#[tokio::test(flavor = "current_thread")]
async fn plugin_digest_sources_are_lazy_and_distinguish_files_from_binaries() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("plugin.toml"),
        r#"
        [plugin]
        id = "digest-test"
        version = "1"
        class = "scraper"
        main = "plugin.js"
        [[plugin.caches]]
        key = "data"
        label = "Data"
        [scraper]
        requires = ["package"]
        provides = ["meta.digest"]
    "#,
    )
    .unwrap();
    std::fs::write(root.path().join("plugin.js"), r#"
        import directory from "pkg:assets";
        import { caches } from "polymer:cache";
        export function describe({package: pkg}) {
            if ("binaryDigest" in pkg || "__digestPath" in pkg || "__digestToken" in pkg) throw new Error("private data escaped");
            return { name: pkg.filename, digest: typeof pkg.digest, binary: typeof pkg.binary.digest };
        }
        export async function inspect({package: pkg}) {
            if (pkg.details.__digestPath !== "ordinary metadata" || pkg.details.binaryDigest !== "ordinary value" || "digest" in pkg.details) {
                throw new Error("package construction rewrote unrelated metadata");
            }
            const files = [];
            for await (const entry of directory) files.push(entry);
            const file = files.find(entry => entry.name === "headered.nes");
            const algorithms = ["sha2", "sha1", "md5", "crc32", "blake3"];
            const results = [];
            for (const algorithm of algorithms) results.push([
                await file.digest(algorithm), await pkg.digest(algorithm), await pkg.binary.digest(algorithm)
            ]);
            const cache = caches.data.open("bytes", { read: false, write: true });
            const writer = cache.writable.getWriter();
            await writer.write("123456789"); await writer.close();
            if (await cache.digest("crc32") !== "cbf43926") throw new Error("wrong cache digest");
            for (const source of [file, cache, pkg, pkg.binary]) {
                let rejected = false;
                try { await source.digest("sha256"); } catch { rejected = true; }
                if (!rejected) throw new Error("invalid algorithm accepted");
            }
            return results;
        }
        export async function* onScrape(input) {
            const { packages } = input;
            if (Object.keys(input).some(key => key.startsWith("package.hash."))) throw new Error("eager hash exposed");
            for (const pkg of packages) {
                if ("binaryDigest" in pkg) throw new Error("private identity exposed");
                if ("bytes" in pkg.files[0] || "text" in pkg.files[0] || "binary" in pkg.files[0]) throw new Error("unexpected scraper file API");
                yield { key: "meta.digest", value: await pkg.files[0].digest("crc32"), source: pkg.files[0].name };
            }
        }
    "#).unwrap();
    std::fs::create_dir(root.path().join("assets")).unwrap();
    let path = root.path().join("assets/headered.nes");
    let mut image = vec![0; 16];
    image[..4].copy_from_slice(b"NES\x1a");
    image.extend_from_slice(b"123456789");
    std::fs::write(&path, &image).unwrap();
    let registry = PluginRegistry::new();
    registry.load(root.path()).unwrap();
    let storage = tempfile::tempdir().unwrap();
    let runtime = PluginRuntime::new(
        registry,
        polymer_library::Directories::rooted(storage.path(), storage.path()),
        PluginRuntimeOptions::default(),
    );
    let package = serde_json::json!({
        "__digestPath": path, "filename": "headered.nes", "binaryDigest": "host-only",
        "details": { "__digestPath": "ordinary metadata", "binaryDigest": "ordinary value" },
        "files": [{ "__digestPath": path, "name": "headered.nes", "size": image.len() }]
    });
    let results = runtime
        .handle()
        .invoke(
            "digest-test",
            &["inspect".into()],
            vec![serde_json::json!({"package": package})],
        )
        .await
        .unwrap();
    for (index, algorithm) in [
        DigestAlgorithm::Sha2,
        DigestAlgorithm::Sha1,
        DigestAlgorithm::Md5,
        DigestAlgorithm::Crc32,
        DigestAlgorithm::Blake3,
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(results[index][0], path.as_path().digest(algorithm).unwrap());
        assert_eq!(results[index][0], results[index][1]);
        assert_eq!(
            results[index][2],
            PackageBinary(&path).digest(algorithm).unwrap()
        );
        assert_ne!(results[index][0], results[index][2]);
    }
    let input = polymer_pluginhost::scraper::ScrapeInput {
        metadata: Default::default(),
        packages: vec![package],
    };
    let scraped = runtime
        .handle()
        .scrape("digest-test", input, runtime.handle().cancellation_token())
        .await
        .unwrap();
    assert_eq!(scraped.len(), 1);
    // A package whose backing file is missing can still be passed without hashing.
    let missing = root.path().join("missing.nes");
    let result = runtime.handle().invoke("digest-test", &["describe".into()], vec![serde_json::json!({
        "package": {"__digestPath": missing, "filename": "missing.nes", "binaryDigest": "private"}
    })]).await.unwrap();
    assert_eq!(
        result,
        serde_json::json!({"name": "missing.nes", "digest": "function", "binary": "function"})
    );
}

#[test]
fn raw_files_and_normalized_binaries_have_distinct_digests() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("headered.nes");
    let mut image = vec![0; 16];
    image[..4].copy_from_slice(b"NES\x1a");
    image.extend_from_slice(b"123456789");
    std::fs::write(&path, &image).unwrap();
    let payload = directory.path().join("payload");
    std::fs::write(&payload, b"123456789").unwrap();
    let vectors = [
        (
            DigestAlgorithm::Sha2,
            "15e2b0d3c33891ebb0f1ef609ec419420c20e320ce94c65fbc8c3312448eb225",
        ),
        (
            DigestAlgorithm::Sha1,
            "f7c3bc1d808e04732adf679965ccc34ca7ae3441",
        ),
        (DigestAlgorithm::Md5, "25f9e794323b453885f5181f1b624d0b"),
        (DigestAlgorithm::Crc32, "cbf43926"),
    ];
    for (algorithm, expected) in vectors {
        assert_eq!(payload.as_path().digest(algorithm).unwrap(), expected);
        assert_eq!(PackageBinary(&path).digest(algorithm).unwrap(), expected);
        assert_ne!(path.as_path().digest(algorithm).unwrap(), expected);
    }
    assert_eq!(
        PackageBinary(&path)
            .digest(DigestAlgorithm::Blake3)
            .unwrap(),
        blake3::hash(b"123456789").to_hex().as_str()
    );
    // Repeated requests observe current bytes rather than a cached digest.
    std::fs::write(&payload, b"changed").unwrap();
    assert_ne!(
        payload.as_path().digest(DigestAlgorithm::Crc32).unwrap(),
        "cbf43926"
    );
}
