use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=dll/arm64/SDL3.dll");
    println!("cargo:rerun-if-changed=dll/x86_64/SDL3.dll");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let architecture = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86_64") => "x86_64",
        _ => return,
    };
    let source = PathBuf::from("dll").join(architecture).join("SDL3.dll");
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo provides OUT_DIR"));
    let profile_dir = output
        .ancestors()
        .nth(3)
        .expect("OUT_DIR is nested under the target profile directory");
    fs::copy(&source, profile_dir.join("SDL3.dll"))
        .unwrap_or_else(|error| panic!("failed to stage {}: {error}", source.display()));
}
