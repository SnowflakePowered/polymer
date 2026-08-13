use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::Arc,
};
use swc::{Compiler, HandlerOpts, config::Options, try_with_handler};
use swc_common::{GLOBALS, Globals, SourceMap};

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // The current-thread Tokio + winit bootstrap owns several large GPU
        // initialization futures before the event loop begins. Debug builds
        // retain enough of those frames to exhaust the PE default stack.
        println!("cargo:rustc-link-arg-bin=snowglass-shell-webview2=/STACK:8388608");
    }
    println!("cargo:rerun-if-changed=src/js/bridge.ts");
    println!("cargo:rerun-if-changed=src/js/remote_value_codec.js");
    println!("cargo:rerun-if-changed=../../crates/runtime/snowglass-pluginhost/bus_client.js");
    println!("cargo:rerun-if-changed=src/js/plugin_facade.js");

    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let bridge = output.join("bridge.js");
    compile(&manifest, "src/js/bridge.ts", bridge.clone());
    let shared_bus = fs::read_to_string(
        manifest.join("../../crates/runtime/snowglass-pluginhost/bus_client.js"),
    )
    .unwrap();
    let remote_values = fs::read_to_string(manifest.join("src/js/remote_value_codec.js")).unwrap();
    let facade = fs::read_to_string(manifest.join("src/js/plugin_facade.js")).unwrap();
    let compiled = fs::read_to_string(&bridge).unwrap();
    fs::write(
        bridge,
        format!("{shared_bus}\n{facade}\n{remote_values}\n{compiled}"),
    )
    .unwrap();
}

fn compile(manifest: &Path, source: &str, output: PathBuf) {
    let source = manifest.join(source);
    let source_map: Arc<SourceMap> = Default::default();
    let source_file = source_map
        .load_file(&source)
        .unwrap_or_else(|error| panic!("failed to load {}: {error}", source.display()));
    let options: Options = serde_json::from_value(serde_json::json!({
        "swcrc": false,
        "filename": source.to_string_lossy(),
        "jsc": {
            "parser": { "syntax": "typescript" },
            "target": "es2022"
        },
        "isModule": false,
        "minify": false
    }))
    .expect("the embedded SWC options must be valid");
    let compiler = Compiler::new(source_map.clone());
    let compiled = GLOBALS.set(&Globals::new(), || {
        try_with_handler(source_map, HandlerOpts::default(), |handler| {
            compiler.process_js_file(source_file, handler, &options)
        })
    });
    let compiled = compiled
        .unwrap_or_else(|error| panic!("SWC failed to compile {}: {error:?}", source.display()));
    fs::write(&output, compiled.code)
        .unwrap_or_else(|error| panic!("failed to write {}: {error}", output.display()));
}
