#![cfg(target_os = "macos")]
use std::{path::PathBuf, process::Command};

#[test]
#[ignore = "requires a logged-in WindowServer session and POLYMER_APPKIT_HOOK pointing to the built dylib"]
fn injected_windows_are_ordered_but_unreachable() {
    let hook =
        PathBuf::from(std::env::var_os("POLYMER_APPKIT_HOOK").expect("set POLYMER_APPKIT_HOOK"));
    assert!(hook.is_file());
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("window");
    let mut compiler = Command::new("xcrun");
    compiler.args(["clang", "-fobjc-arc", "-framework", "Cocoa"]);
    if let Some(sdk) = std::env::var_os("SDKROOT") {
        compiler.arg("-isysroot").arg(sdk);
    }
    let output = compiler
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/window.m"))
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new(executable)
        .env("POLYMER_DESKTOP_ISOLATION", "1")
        .env("DYLD_INSERT_LIBRARIES", hook)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
