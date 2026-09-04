use std::env;

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let architecture = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let dll = match architecture.as_str() {
        "x86_64" => "winfsp-x64.dll",
        "x86" => "winfsp-x86.dll",
        "aarch64" => "winfsp-a64.dll",
        other => panic!("unsupported WinFSP architecture '{other}'"),
    };
    match (
        env::var("CARGO_CFG_TARGET_ENV").as_deref(),
        env::var("CARGO_CFG_TARGET_ABI").as_deref(),
    ) {
        (Ok("msvc"), _) => {
            println!("cargo:rustc-link-lib=dylib=delayimp");
            println!("cargo:rustc-link-arg=/DELAYLOAD:{dll}");
        }
        (Ok("gnu"), Ok("llvm")) => println!("cargo:rustc-link-arg=-Wl,--delayload={dll}"),
        (environment, abi) => {
            panic!("unsupported WinFSP linker environment {environment:?}/{abi:?}")
        }
    }
}
