use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let architecture = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86_64") => "x86_64",
        Ok(other) => panic!("SDL3 input is unsupported on {other}"),
        Err(error) => panic!("target architecture is unavailable: {error}"),
    };
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let dll = manifest
        .join("../../hooks/snowglass-hook-sdl3/dll")
        .join(architecture)
        .join("SDL3.dll");
    println!("cargo:rerun-if-changed={}", dll.display());
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let definition = output.join("SDL3.def");
    write_definition(&dll, &definition);

    let target = env::var("TARGET").unwrap();
    let lib = cc::windows_registry::find_tool(&target, "lib.exe")
        .unwrap_or_else(|| panic!("Visual Studio lib.exe is required to create SDL3.lib"));
    let machine = if architecture == "arm64" {
        "ARM64"
    } else {
        "X64"
    };
    let status = Command::new(lib.path())
        .arg(format!("/def:{}", definition.display()))
        .arg(format!("/out:{}", output.join("SDL3.lib").display()))
        .arg(format!("/machine:{machine}"))
        .arg("/nologo")
        .status()
        .expect("failed to run lib.exe");
    assert!(status.success(), "lib.exe failed to create SDL3.lib");
    println!("cargo:rustc-link-search=native={}", output.display());

    // Match the existing hook build: binaries can load the exact DLL from
    // which their import library was generated.
    let profile_dir = output.ancestors().nth(3).expect("Cargo OUT_DIR layout");
    fs::copy(&dll, profile_dir.join("SDL3.dll")).expect("failed to stage SDL3.dll");
}

fn write_definition(dll: &Path, output: &Path) {
    let bytes = fs::read(dll).expect("failed to read SDL3.dll");
    let pe = u32_at(&bytes, 0x3c) as usize;
    assert_eq!(&bytes[pe..pe + 4], b"PE\0\0", "invalid PE signature");
    let sections = u16_at(&bytes, pe + 6) as usize;
    let optional_size = u16_at(&bytes, pe + 20) as usize;
    let optional = pe + 24;
    let magic = u16_at(&bytes, optional);
    let data_directories = optional
        + match magic {
            0x20b => 112, // PE32+
            0x10b => 96,  // PE32
            _ => panic!("unknown PE optional-header magic {magic:#x}"),
        };
    let export_rva = u32_at(&bytes, data_directories);
    assert_ne!(export_rva, 0, "SDL3.dll has no export table");
    let section_table = optional + optional_size;
    let rva_to_offset = |rva: u32| -> usize {
        for index in 0..sections {
            let section = section_table + index * 40;
            let virtual_size = u32_at(&bytes, section + 8);
            let virtual_address = u32_at(&bytes, section + 12);
            let raw_size = u32_at(&bytes, section + 16);
            let raw_offset = u32_at(&bytes, section + 20);
            let size = virtual_size.max(raw_size);
            if (virtual_address..virtual_address.saturating_add(size)).contains(&rva) {
                return (raw_offset + rva - virtual_address) as usize;
            }
        }
        panic!("PE RVA {rva:#x} is outside every section")
    };
    let directory = rva_to_offset(export_rva);
    let name_count = u32_at(&bytes, directory + 24) as usize;
    let names = rva_to_offset(u32_at(&bytes, directory + 32));
    let mut definition = String::from("LIBRARY SDL3.dll\nEXPORTS\n");
    for index in 0..name_count {
        let name = rva_to_offset(u32_at(&bytes, names + index * 4));
        let end = bytes[name..].iter().position(|byte| *byte == 0).unwrap() + name;
        definition.push_str("    ");
        definition.push_str(std::str::from_utf8(&bytes[name..end]).expect("non-UTF8 export"));
        definition.push('\n');
    }
    fs::write(output, definition).expect("failed to write SDL3.def");
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
