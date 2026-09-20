#![cfg(target_os = "macos")]
use polymer_platform_macos::StagedBundle;
use std::{fs, process::Command};

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
                plist::Value::String("org.polymer.staging-test".into()),
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
    assert!(!staged_path.exists());
    assert!(source.exists());
}
