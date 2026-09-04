use super::*;

#[test]
fn temporary_scratch_is_materialized_under_the_configured_root() {
    let package = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let scratch_root = storage.path().join("plugin-scratch");
    let mut options = FilesystemOptions::new(package.path().to_owned());
    options.scratch_root = Some(scratch_root.clone());
    let mut state = FilesystemState::new(options).unwrap();
    let ResourceHandle::Tree(handle) = state.scratch_resource("temporary").unwrap() else {
        unreachable!()
    };

    let _materialized = state.volume(handle).unwrap().materialize().unwrap();
    let entries = std::fs::read_dir(scratch_root)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert!(
        entries[0]
            .file_name()
            .to_string_lossy()
            .starts_with("snowglass-scratch-")
    );
}

#[test]
fn declared_cache_is_lazy_persistent_and_reused() {
    let package = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let cache_path = storage.path().join("plugin-cache");
    let mut options = FilesystemOptions::new(package.path().to_owned());
    options.caches.push(NamedCache {
        key: "shaders".to_owned(),
        path: cache_path.clone(),
    });
    let mut state = FilesystemState::new(options).unwrap();

    assert!(!cache_path.exists());
    let ResourceHandle::Tree(first) = state.cache("shaders").unwrap() else {
        unreachable!()
    };
    let ResourceHandle::Tree(second) = state.cache("shaders").unwrap() else {
        unreachable!()
    };
    assert_eq!(first, second);
    assert!(cache_path.is_dir());

    let resource = state.volume(first).unwrap();
    assert_eq!(resource.authority(), Permissions::ALL);
    assert_eq!(
        resource.origin(),
        &ResourceOrigin::Cache("shaders".to_owned())
    );
    let materialized = resource.materialize().unwrap();
    assert!(
        !materialized
            .backend
            .root()
            .unwrap()
            .metadata()
            .unwrap()
            .readonly
    );
    assert!(state.cache("undeclared").is_err());
}

#[test]
fn javascript_mount_cache_returns_a_mountable_tree_capability() {
    let package = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let cache_path = storage.path().join("plugin-cache");
    let mut options = FilesystemOptions::new(package.path().to_owned());
    options.caches.push(NamedCache {
        key: "shaders".to_owned(),
        path: cache_path.clone(),
    });
    let mut runtime = JsRuntime::new(deno_core::RuntimeOptions {
        extensions: vec![extension(options)],
        ..Default::default()
    });

    runtime
        .execute_script(
            "snowglass:test/cache.js",
            r#"
                    const cache = __snowglassFilesystem.filesystem.mountCache("shaders");
                    __snowglassFilesystem.filesystem.mountTree(
                      "/Cache", cache, { mode: "rwx" });
                "#,
        )
        .unwrap();

    let prepared = take_prepared(&mut runtime).unwrap().unwrap();
    assert_eq!(prepared.filesystem().compiled().bindings().len(), 1);
    assert!(cache_path.is_dir());
}

#[test]
fn javascript_mount_storage_preserves_root_and_format_requests() {
    fn runtime_with_storage(package: &Path) -> JsRuntime {
        let mut options = FilesystemOptions::new(package.to_owned());
        options.storage.insert(
            "card1".into(),
            Arc::new(ScratchVolumeFactory::new(ScratchBacking::Memory)),
        );
        JsRuntime::new(deno_core::RuntimeOptions {
            extensions: vec![extension(options)],
            ..Default::default()
        })
    }

    let package = tempfile::tempdir().unwrap();
    let mut root = runtime_with_storage(package.path());
    root.execute_script(
        "snowglass:test/storage-root.js",
        r#"
                const card = __snowglassFilesystem.storage.card1;
                __snowglassFilesystem.filesystem.mountStorage(
                  "/Storage", card, { mode: "rwx" });
            "#,
    )
    .unwrap();
    let prepared = take_prepared(&mut root).unwrap().unwrap();
    assert!(matches!(
        prepared.filesystem().compiled().bindings()[0].kind(),
        snowglass_vfs::BindingKind::Storage { format: None, .. }
    ));
    prepared.filesystem().materialize().unwrap();

    let mut formatted = runtime_with_storage(package.path());
    formatted
        .execute_script(
            "snowglass:test/storage-format.js",
            r#"
                    const card = __snowglassFilesystem.storage.card1;
                    __snowglassFilesystem.filesystem.mountStorage(
                      "/Storage/card.mcd", card, { mode: "rw", format: "raw" });
                "#,
        )
        .unwrap();
    let prepared = take_prepared(&mut formatted).unwrap().unwrap();
    assert!(matches!(
        prepared.filesystem().compiled().bindings()[0].kind(),
        snowglass_vfs::BindingKind::Storage { format: Some(format), .. } if format == "raw"
    ));
    assert!(matches!(
        prepared.filesystem().materialize(),
        Err(VfsError::Unsupported(reason)) if reason.contains("does not support")
    ));
}
