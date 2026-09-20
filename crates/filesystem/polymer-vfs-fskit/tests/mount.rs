#![cfg(target_os = "macos")]
use polymer_vfs::{
    FilesystemHost, FilesystemPlan, FinalizationReason, Permissions, Resource, ResourceOrigin,
    ScratchVolumeFactory, VfsPathBuf,
};
use polymer_vfs_fskit::FsKitFilesystemHost;
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    sync::Arc,
};

#[test]
#[ignore = "requires the installed/enabled FSKit extension and a signed test host with the FSKit mount entitlement"]
fn mounted_files_delegate_mutations_to_the_live_vfs() {
    let host = FsKitFilesystemHost::new("gg.polymer.filesystem");
    assert!(host.supported(), "enable the Polymer FSKit extension first");
    let native = tempfile::tempdir().unwrap();
    let mut plan = FilesystemPlan::default();
    plan.mount_tree(
        VfsPathBuf::root(),
        Arc::new(Resource::new(
            Arc::new(ScratchVolumeFactory::new(
                polymer_vfs::ScratchBacking::Memory,
            )),
            Permissions::ALL,
            ResourceOrigin::Scratch,
        )),
        Permissions::ALL,
    );
    plan.mount_tree(
        VfsPathBuf::new("/host").unwrap(),
        Arc::new(Resource::new(
            Arc::new(polymer_vfs::HostDirectoryFactory::new(
                native.path().to_owned(),
                true,
            )),
            Permissions::ALL,
            ResourceOrigin::Scratch,
        )),
        Permissions::ALL,
    );
    let filesystem = Arc::new(plan.seal().unwrap());
    let endpoint = host.allocate_endpoint();
    let root = std::path::PathBuf::from(endpoint.project(VfsPathBuf::root().as_path()));
    let mounted = host.mount(endpoint, filesystem.clone()).unwrap();
    fs::create_dir(root.join("saves")).unwrap();
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(root.join("saves/slot"))
        .unwrap();
    file.write_all(b"first state").unwrap();
    file.sync_all().unwrap();
    fs::rename(root.join("saves/slot"), root.join("saves/renamed")).unwrap();
    file.seek(SeekFrom::Start(6)).unwrap();
    file.write_all(b"save!").unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"first save!");
    drop(file);
    let path = VfsPathBuf::new("/saves/renamed").unwrap();
    let handle = filesystem
        .open(
            &path,
            polymer_vfs::OpenOptions {
                read: true,
                ..Default::default()
            },
        )
        .unwrap();
    let mut bytes = [0; 11];
    assert_eq!(handle.read(0, &mut bytes).unwrap(), 11);
    assert_eq!(&bytes, b"first save!");
    fs::remove_file(root.join("saves/renamed")).unwrap();
    assert!(fs::read_dir(root.join("saves")).unwrap().next().is_none());
    // Executable mappings must remain readable after the loader closes its FD.
    let executable = root.join("true");
    fs::write(&executable, fs::read("/usr/bin/true").unwrap()).unwrap();
    let status = std::process::Command::new(&executable).status().unwrap();
    assert!(status.success(), "executing through FSKit failed: {status}");
    fs::remove_file(&executable).unwrap();
    let executable = root.join("native-guest");
    let source = native.path().join("guest.c");
    fs::write(&source, "int main(void) { return 37; }\n").unwrap();
    let binary = native.path().join("guest");
    assert!(
        std::process::Command::new("xcrun")
            .arg("clang")
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .status()
            .unwrap()
            .success()
    );
    fs::write(&executable, fs::read(binary).unwrap()).unwrap();
    let status = std::process::Command::new(&executable).status().unwrap();
    assert_eq!(
        status.code(),
        Some(37),
        "native guest execution failed: {status}"
    );
    fs::remove_file(executable).unwrap();
    let status = std::process::Command::new(root.join("host/guest"))
        .status()
        .unwrap();
    assert_eq!(
        status.code(),
        Some(37),
        "host-backed execution failed: {status}"
    );
    mounted.finish(FinalizationReason::ProcessExited).unwrap();
    // Checking a child path can hide a disconnected mount returning EIO.
    // FSKit removes its generated mount point after a successful unmount.
    assert_eq!(
        fs::metadata(&root).unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
}
