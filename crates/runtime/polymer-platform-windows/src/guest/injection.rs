use super::GuestPlatform;
use crate::{ProcessArchitecture, ProcessId};
use std::{
    ffi::CStr,
    path::{Path, PathBuf},
    process::Command,
};

/// One DLL and installer export to invoke before a target's initial thread is
/// resumed. Relative DLL paths are resolved inside the target architecture's
/// artifact directory.
#[derive(Clone, Debug)]
pub(super) struct InjectionLibrary {
    name: String,
    library: PathBuf,
    export: &'static CStr,
}

impl InjectionLibrary {
    pub(super) fn new(
        name: impl Into<String>,
        library: impl Into<PathBuf>,
        export: &'static CStr,
    ) -> Self {
        Self {
            name: name.into(),
            library: library.into(),
            export,
        }
    }
}

impl GuestPlatform {
    pub(super) fn artifact_dir(artifact_root: &Path, architecture: ProcessArchitecture) -> PathBuf {
        let packaged = artifact_root.join(architecture.executable_suffix());
        if packaged.is_dir() {
            return packaged;
        }
        let target = match architecture {
            ProcessArchitecture::X86_64 => "x86_64-pc-windows-msvc",
            ProcessArchitecture::Arm64 => "aarch64-pc-windows-msvc",
        };
        let cargo = artifact_root
            .parent()
            .zip(artifact_root.file_name())
            .map(|(target_dir, profile)| target_dir.join(target).join(profile));
        cargo
            .filter(|directory| directory.is_dir())
            .unwrap_or_else(|| artifact_root.to_path_buf())
    }
}

impl InjectionLibrary {
    pub(super) fn resolve(
        artifact_dir: &Path,
        libraries: &[InjectionLibrary],
    ) -> std::io::Result<Vec<InjectionLibrary>> {
        libraries
            .iter()
            .map(|library| {
                let path = if library.library.is_absolute() {
                    library.library.clone()
                } else {
                    artifact_dir.join(&library.library)
                };
                if !path.is_file() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!("{} hook is missing at {}", library.name, path.display()),
                    ));
                }
                Ok(InjectionLibrary {
                    name: library.name.clone(),
                    library: path,
                    export: library.export,
                })
            })
            .collect()
    }

    pub(super) fn inject(
        artifact_root: &Path,
        artifact_dir: &Path,
        architecture: ProcessArchitecture,
        process_id: ProcessId,
        libraries: &[InjectionLibrary],
    ) -> std::io::Result<()> {
        let suffixed = artifact_root.join(format!(
            "polymer-injector-{}.exe",
            architecture.executable_suffix()
        ));
        let architecture_local = artifact_root
            .join(architecture.executable_suffix())
            .join("polymer-injector.exe");
        let target_local = artifact_dir.join("polymer-injector.exe");
        let generic = artifact_root.join("polymer-injector.exe");
        let injector = if suffixed.is_file() {
            suffixed.clone()
        } else if architecture_local.is_file() {
            architecture_local
        } else if target_local.is_file() {
            target_local
        } else if generic.is_file()
            && matches!(
                (std::env::consts::ARCH, architecture),
                ("x86_64", ProcessArchitecture::X86_64) | ("aarch64", ProcessArchitecture::Arm64)
            )
        {
            generic
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "{} injector is missing at {}",
                    architecture.executable_suffix(),
                    suffixed.display()
                ),
            ));
        };
        let mut command = Command::new(&injector);
        command.arg("--pid").arg(process_id.0.to_string());
        for library in libraries {
            command
                .arg("--library")
                .arg(&library.library)
                .arg("--export")
                .arg(library.export.to_string_lossy().as_ref());
        }
        let status = command.status()?;
        if status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(format!(
                "{} failed with {status}",
                injector.display()
            )))
        }
    }
}
