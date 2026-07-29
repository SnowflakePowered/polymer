use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::Arc,
};
use swc::{Compiler, HandlerOpts, config::Options, try_with_handler};
use swc_common::{GLOBALS, Globals, SourceMap};

fn main() {
    println!("cargo:rerun-if-changed=src/js/bridge.ts");

    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    compile(&manifest, "src/js/bridge.ts", output.join("bridge.js"));
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
