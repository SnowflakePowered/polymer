fn main() {
    println!("cargo:rerun-if-changed=src/xpc/protocol.m");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new()
            .file("src/xpc/protocol.m")
            .flag("-fobjc-arc")
            .compile("polymer_xpc_protocol");
    }
}
