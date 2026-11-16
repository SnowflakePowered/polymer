use polymer_vfs::{
    CreateOptions, FilesystemPlan, HostDirectoryFactory, MemoryFileFactory, OpenOptions,
    Permissions, Resource, ResourceOrigin, VfsError,
};
use std::sync::Arc;

#[test]
fn missing_mount_parents_allow_sibling_writes_in_either_order() {
    let permissions = Permissions::READ | Permissions::WRITE | Permissions::EXECUTE;
    for reverse in [false, true] {
        let mut plan = FilesystemPlan::default();
        let mut paths = ["/User/Config/settings.ini", "/User/Logs/output.log"];
        if reverse {
            paths.reverse();
        }
        for path in paths {
            plan.mount_file(
                path.parse().unwrap(),
                Arc::new(Resource::new(
                    Arc::new(MemoryFileFactory::immutable(b"mounted".to_vec())),
                    Permissions::READ,
                    ResourceOrigin::Memory,
                )),
                Permissions::READ,
            );
        }
        let filesystem = plan.seal().unwrap();
        filesystem.materialize().unwrap();
        for parent in ["", "/User", "/User/Config", "/User/Logs"] {
            let path = format!("{parent}/sibling").parse().unwrap();
            filesystem
                .create_file(&path, CreateOptions::default())
                .unwrap();
            let file = filesystem
                .open(
                    &path,
                    OpenOptions {
                        read: true,
                        write: true,
                        truncate: false,
                    },
                )
                .unwrap();
            file.write(0, b"scratch").unwrap();
            let mut bytes = [0; 7];
            assert_eq!(file.read(0, &mut bytes).unwrap(), bytes.len());
            assert_eq!(&bytes, b"scratch");
            filesystem
                .create_dir(&format!("{parent}/directory").parse().unwrap())
                .unwrap();
        }
        for path in paths {
            let file = filesystem
                .open(
                    &path.parse().unwrap(),
                    OpenOptions {
                        read: true,
                        write: false,
                        truncate: false,
                    },
                )
                .unwrap();
            let mut bytes = [0; 7];
            file.read(0, &mut bytes).unwrap();
            assert_eq!(&bytes, b"mounted");
            assert!(
                filesystem
                    .open(
                        &path.parse().unwrap(),
                        OpenOptions {
                            read: true,
                            write: true,
                            truncate: false,
                        }
                    )
                    .is_err()
            );
        }
        assert_eq!(
            filesystem
                .compiled()
                .resolve(&"/User/Config".parse().unwrap())
                .unwrap()
                .unwrap()
                .permissions(),
            permissions
        );
    }
}

#[test]
fn explicit_parent_keeps_its_contents_and_permissions_in_either_order() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("Config")).unwrap();
    std::fs::write(root.path().join("Config/existing.ini"), b"existing").unwrap();
    for parent_first in [false, true] {
        let mut plan = FilesystemPlan::default();
        for mount_parent in [parent_first, !parent_first] {
            if mount_parent {
                let permissions = Permissions::READ | Permissions::EXECUTE;
                plan.mount_tree(
                    "/User".parse().unwrap(),
                    Arc::new(Resource::new(
                        Arc::new(HostDirectoryFactory::new(root.path().to_owned(), false)),
                        permissions,
                        ResourceOrigin::HostFile,
                    )),
                    permissions,
                );
            } else {
                plan.mount_file(
                    "/User/Config/settings.ini".parse().unwrap(),
                    Arc::new(Resource::new(
                        Arc::new(MemoryFileFactory::immutable(b"override".to_vec())),
                        Permissions::READ,
                        ResourceOrigin::Memory,
                    )),
                    Permissions::READ,
                );
            }
        }
        let filesystem = plan.seal().unwrap();
        filesystem.materialize().unwrap();
        for (name, expected) in [("existing.ini", b"existing"), ("settings.ini", b"override")] {
            let file = filesystem
                .open(
                    &format!("/User/Config/{name}").parse().unwrap(),
                    OpenOptions {
                        read: true,
                        write: false,
                        truncate: false,
                    },
                )
                .unwrap();
            let mut bytes = [0; 8];
            file.read(0, &mut bytes).unwrap();
            assert_eq!(&bytes, expected);
        }
        assert!(matches!(
            filesystem.create_file(
                &"/User/Config/new.ini".parse().unwrap(),
                CreateOptions::default()
            ),
            Err(VfsError::PermissionDenied(_))
        ));
    }
}

#[test]
fn directories_holding_previously_written_files_can_be_renamed() {
    let permissions = Permissions::READ | Permissions::WRITE | Permissions::EXECUTE;
    let root = tempfile::tempdir().unwrap();
    let mut plan = FilesystemPlan::default();
    plan.mount_tree(
        "/NAND".parse().unwrap(),
        Arc::new(Resource::new(
            Arc::new(HostDirectoryFactory::new(root.path().to_owned(), true)),
            permissions,
            ResourceOrigin::HostFile,
        )),
        permissions,
    );
    let filesystem = plan.seal().unwrap();
    filesystem.materialize().unwrap();
    for directory in [
        "/NAND/staging",
        "/NAND/staging/flash0",
        "/NAND/staging/flash0/font",
    ] {
        filesystem.create_dir(&directory.parse().unwrap()).unwrap();
    }
    let path = "/NAND/staging/flash0/font/ltn0.pgf".parse().unwrap();
    filesystem
        .create_file(&path, CreateOptions::default())
        .unwrap();
    filesystem
        .open(
            &path,
            OpenOptions {
                read: true,
                write: true,
                truncate: false,
            },
        )
        .unwrap()
        .write(0, b"font")
        .unwrap();
    filesystem
        .rename(
            &"/NAND/staging/flash0".parse().unwrap(),
            &"/NAND/flash0".parse().unwrap(),
        )
        .unwrap();
    assert_eq!(
        std::fs::read(root.path().join("flash0/font/ltn0.pgf")).unwrap(),
        b"font"
    );
}
