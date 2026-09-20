#![cfg(target_os = "macos")]
use polymer_platform_macos::{BundleFactory, StagedBundle};
use polymer_vfs::{Node, ResourceFactory, VfsComponent};
use std::{fs, os::unix::fs::MetadataExt, process::Command};

#[test]
fn staging_preserves_entitlements_and_leaves_the_original_untouched() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("Emulator.app");
    fs::create_dir_all(source.join("Contents/MacOS")).unwrap();
    fs::copy("/usr/bin/true", source.join("Contents/MacOS/Emulator")).unwrap();
    let info = plist::Value::Dictionary(
        [
            (
                "CFBundleExecutable".to_owned(),
                plist::Value::String("Emulator".into()),
            ),
            (
                "CFBundleIdentifier".to_owned(),
                plist::Value::String("gg.polymer.staging-test".into()),
            ),
            (
                "CFBundlePackageType".to_owned(),
                plist::Value::String("APPL".into()),
            ),
        ]
        .into_iter()
        .collect(),
    );
    info.to_file_xml(source.join("Contents/Info.plist"))
        .unwrap();
    fs::create_dir_all(source.join("Contents/Resources")).unwrap();
    fs::write(source.join("Contents/Resources/data"), b"bundle data").unwrap();
    // Bundle enumeration must work above macOS's default 256-descriptor limit.
    for index in 0..512 {
        fs::write(
            source.join(format!("Contents/Resources/config-{index}")),
            b"config",
        )
        .unwrap();
    }
    std::os::unix::fs::symlink("Resources", source.join("Contents/ResourceAlias")).unwrap();
    // Framework symlinks can have permissions that differ from the process umask.
    assert!(
        Command::new("/bin/chmod")
            .args(["-h", "700"])
            .arg(source.join("Contents/ResourceAlias"))
            .status()
            .unwrap()
            .success()
    );
    let entitlements = root.path().join("original.plist");
    plist::Value::Dictionary(
        [(
            "com.apple.security.cs.allow-jit".to_owned(),
            plist::Value::Boolean(true),
        )]
        .into_iter()
        .collect(),
    )
    .to_file_xml(&entitlements)
    .unwrap();
    let output = Command::new("/usr/bin/codesign")
        .args(["-f", "-s", "-", "--options", "runtime", "--entitlements"])
        .arg(entitlements)
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let before = fs::read(source.join("Contents/MacOS/Emulator")).unwrap();
    let staged_path;
    {
        let staged = StagedBundle::prepare(&source, root.path()).unwrap();
        staged_path = staged.path().to_owned();
        assert_eq!(
            before,
            fs::read(source.join("Contents/MacOS/Emulator")).unwrap()
        );
        let output = Command::new("/usr/bin/codesign")
            .args(["-d", "--entitlements", "-", "--xml"])
            .arg(staged.path())
            .output()
            .unwrap();
        assert!(output.status.success());
        let plist = plist::Value::from_reader(std::io::Cursor::new(output.stdout)).unwrap();
        let values = plist.as_dictionary().unwrap();
        for key in [
            "com.apple.security.cs.allow-jit",
            "com.apple.security.cs.allow-dyld-environment-variables",
            "com.apple.security.cs.disable-library-validation",
        ] {
            assert_eq!(values[key].as_boolean(), Some(true));
        }
        assert!(
            Command::new(staged.executable())
                .status()
                .unwrap()
                .success()
        );
    }
    let signed_executable = staged_path.join("Contents/MacOS/Emulator");
    let signed_metadata = fs::metadata(&signed_executable).unwrap();
    let reused = StagedBundle::prepare(&source, root.path()).unwrap();
    assert_eq!(reused.path(), staged_path);
    let reused_metadata = fs::metadata(reused.executable()).unwrap();
    assert_eq!(signed_metadata.ino(), reused_metadata.ino());
    assert_eq!(signed_metadata.mtime(), reused_metadata.mtime());
    assert_eq!(signed_metadata.mtime_nsec(), reused_metadata.mtime_nsec());

    // A resource-only update must invalidate the cache even with an unchanged
    // executable and bundle version. Concurrent launches publish the same copy.
    fs::write(source.join("Contents/Resources/data"), b"new content").unwrap();
    let updated = std::thread::scope(|scope| {
        let first = scope.spawn(|| StagedBundle::prepare(&source, root.path()).unwrap());
        let second = scope.spawn(|| StagedBundle::prepare(&source, root.path()).unwrap());
        let first = first.join().unwrap();
        let second = second.join().unwrap();
        assert_eq!(first.path(), second.path());
        first
    });
    assert_ne!(updated.path(), staged_path);
    assert_eq!(
        fs::read(updated.path().join("Contents/Resources/data")).unwrap(),
        b"new content"
    );
    assert_eq!(
        fs::read(staged_path.join("Contents/Resources/data")).unwrap(),
        b"bundle data"
    );
    assert!(
        Command::new(updated.executable())
            .status()
            .unwrap()
            .success()
    );
    // Restore the original resource for the VFS checks below.
    fs::write(source.join("Contents/Resources/data"), b"bundle data").unwrap();
    assert!(source.exists());
    let mounted = BundleFactory::new(source.clone()).materialize().unwrap();
    let root_directory = mounted.backend.root().unwrap();
    let Node::Directory(directory) = root_directory
        .lookup(&VfsComponent::new("Contents").unwrap())
        .unwrap()
        .unwrap()
    else {
        panic!("bundle contents must be a directory");
    };
    let Node::Directory(resources) = directory
        .lookup(&VfsComponent::new("ResourceAlias").unwrap())
        .unwrap()
        .unwrap()
    else {
        panic!("bundle resource alias must expose a directory");
    };
    let Node::File(data) = resources
        .lookup(&VfsComponent::new("data").unwrap())
        .unwrap()
        .unwrap()
    else {
        panic!("bundle resource must expose a file");
    };
    let entries = resources
        .read_dir()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let nodes = entries
        .iter()
        .map(|entry| resources.lookup(&entry.name).unwrap().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(nodes.len(), 513);
    let mut bytes = [0; 11];
    let file = data
        .open(polymer_vfs::OpenOptions {
            read: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(file.read(0, &mut bytes).unwrap(), 11);
    assert_eq!(&bytes, b"bundle data");
    assert_eq!(data.metadata().unwrap().len, 11);
    assert!(data.metadata().unwrap().readonly);
    // The source remains signed and contains its original alias.
    assert!(
        fs::symlink_metadata(source.join("Contents/ResourceAlias"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}
