use std::env;

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // The current-thread Tokio + winit bootstrap owns several large GPU
        // initialization futures before the event loop begins. Debug builds
        // retain enough of those frames to exhaust the PE default stack.
        println!("cargo:rustc-link-arg-bin=snowglass-shell-webview2=/STACK:8388608");
    }
}
