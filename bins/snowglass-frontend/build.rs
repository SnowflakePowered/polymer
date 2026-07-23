fn main() {
    let icon_path = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("icons")
        .join("icon.ico");
    if !icon_path.exists() {
        std::fs::create_dir_all(icon_path.parent().unwrap()).expect("create frontend icon folder");
        let mut directory = ico::IconDir::new(ico::ResourceType::Icon);
        let mut rgba = vec![0_u8; 32 * 32 * 4];
        for pixel in rgba.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[24, 132, 255, 255]);
        }
        let image = ico::IconImage::from_rgba_data(32, 32, rgba);
        directory.add_entry(ico::IconDirEntry::encode(&image).expect("encode frontend icon"));
        directory
            .write(std::fs::File::create(&icon_path).expect("create frontend icon"))
            .expect("write frontend icon");
    }

    let windows = tauri_build::WindowsAttributes::new().window_icon_path(icon_path);
    tauri_build::try_build(tauri_build::Attributes::new().windows_attributes(windows))
        .expect("failed to run Tauri build script");
}
