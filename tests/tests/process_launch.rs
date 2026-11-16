#![cfg(windows)]

use polymer_ipc::input::host::InputIpcListener;
use polymer_platform::{GuestLaunchRequest, Hooks, ProcessExit};
use polymer_platform_windows::GuestPlatform;
use polymer_vfs::{
    FilesystemHost, FilesystemPlan, HostFileFactory, Permissions, Resource, ResourceOrigin,
};
use polymer_vfs_winfsp::WinFspFilesystemHost;
use std::{ffi::OsString, sync::Arc, time::Duration};

#[test]
#[ignore = "requires WinFSP and built Windows hook/injector artifacts"]
fn launch_applies_working_directory_and_environment_on_both_desktops() {
    let desktop = format!("polymer-launch-test-{}", std::process::id());
    for private_desktop in [None, Some(desktop.as_str())] {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("working directory");
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(
            directory.join("probe.cmd"),
            "@echo off\r\ncd > directory.txt\r\necho %POLYMER_LAUNCH_TEST%> environment.txt\r\n",
        )
        .unwrap();
        let host = WinFspFilesystemHost::default();
        let endpoint = host.allocate_endpoint();
        let mut plan = FilesystemPlan::default();
        plan.mount_file(
            "/cmd.exe".parse().unwrap(),
            Arc::new(Resource::new(
                Arc::new(HostFileFactory::new(
                    std::env::var_os("ComSpec").unwrap().into(),
                    false,
                )),
                "rx".parse::<Permissions>().unwrap(),
                ResourceOrigin::HostFile,
            )),
            "rx".parse().unwrap(),
        );
        let filesystem = host
            .mount(endpoint, Arc::new(plan.seal().unwrap()))
            .unwrap();
        let session = format!("{desktop}-{}", private_desktop.is_some());
        let request = GuestLaunchRequest {
            session: session.clone(),
            executable: "/cmd.exe".parse().unwrap(),
            arguments: vec!["/d".into(), "/c".into(), "probe.cmd".into()],
            environment: vec![(
                OsString::from("POLYMER_LAUNCH_TEST"),
                OsString::from("value=with spaces"),
            )],
            working_directory: Some(directory.clone()),
            artifact_root: std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .to_owned(),
            hooks: Hooks {
                graphics: Vec::new(),
                input: Vec::new(),
            },
            isolate: private_desktop.is_some(),
            render_configuration: Default::default(),
        };
        let mut process = GuestPlatform::launch(
            &request,
            &filesystem,
            InputIpcListener::bind(&session).unwrap(),
        )
        .unwrap();
        let exit = process
            .subscribe_exit()
            .unwrap()
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        assert_eq!(exit, ProcessExit::Exited { code: 0 });
        process.terminate_tree_and_wait().unwrap();
        assert_eq!(
            std::fs::read_to_string(directory.join("directory.txt"))
                .unwrap()
                .trim(),
            directory.to_str().unwrap(),
        );
        assert_eq!(
            std::fs::read_to_string(directory.join("environment.txt"))
                .unwrap()
                .trim(),
            "value=with spaces",
        );
    }
}

#[test]
#[ignore = "requires WinFSP"]
fn mounted_directories_holding_closed_files_can_be_renamed() {
    let root = tempfile::tempdir().unwrap();
    let host = WinFspFilesystemHost::default();
    let endpoint = host.allocate_endpoint();
    let mut plan = FilesystemPlan::default();
    let permissions = "rwx".parse::<Permissions>().unwrap();
    plan.mount_tree(
        "/NAND".parse().unwrap(),
        Arc::new(Resource::new(
            Arc::new(polymer_vfs::HostDirectoryFactory::new(
                root.path().to_owned(),
                true,
            )),
            permissions,
            ResourceOrigin::HostFile,
        )),
        permissions,
    );
    let filesystem = host
        .mount(endpoint, Arc::new(plan.seal().unwrap()))
        .unwrap();
    let nand = std::path::PathBuf::from(
        filesystem.endpoint().project(
            "/NAND"
                .parse::<polymer_vfs::VfsPathBuf>()
                .unwrap()
                .as_path(),
        ),
    );
    std::fs::create_dir_all(nand.join("staging/flash0/font")).unwrap();
    std::fs::write(nand.join("staging/flash0/font/ltn0.pgf"), b"font").unwrap();
    std::fs::rename(nand.join("staging/flash0"), nand.join("flash0")).unwrap();
    assert_eq!(
        std::fs::read(root.path().join("flash0/font/ltn0.pgf")).unwrap(),
        b"font"
    );
}
