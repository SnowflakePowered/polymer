use std::env;

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // The current-thread Tokio + winit bootstrap owns several large GPU
        // initialization futures before the event loop begins. Debug builds
        // retain enough of those frames to exhaust the PE default stack.
        println!("cargo:rustc-link-arg-bin=polymer-gui=/STACK:8388608");
        let architecture = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
        let dll = match architecture.as_str() {
            "x86_64" => "winfsp-x64.dll",
            "x86" => "winfsp-x86.dll",
            "aarch64" => "winfsp-a64.dll",
            other => panic!("unsupported WinFSP architecture '{other}'"),
        };
        println!("cargo:rustc-link-lib=dylib=delayimp");
        println!("cargo:rustc-link-arg=/DELAYLOAD:{dll}");
    }
}
