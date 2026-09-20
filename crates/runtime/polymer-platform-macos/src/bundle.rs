//! Prepare a private emulator copy for launch-time dyld injection.

use plist::Value;
use std::{
    io,
    path::{Component, Path, PathBuf},
    process::{Command, Output},
};

/// Owns a freshly cloned and re-signed bundle. Retain it until the guest exits.
/// Stage before adding the bundle to the mandatory guest filesystem; this is
/// preparation, not a launch API or a way to bypass the VFS.
pub struct StagedBundle {
    _directory: tempfile::TempDir,
    bundle: PathBuf,
    executable: PathBuf,
}

impl StagedBundle {
    /// Each preparation uses a fresh APFS clone, so upstream updates cannot leave
    /// a stale signed executable in a cache. Never modifies the source bundle.
    pub fn prepare(source: &Path, staging_root: &Path) -> io::Result<Self> {
        let source = source.canonicalize()?;
        let info =
            Value::from_file(source.join("Contents/Info.plist")).map_err(io::Error::other)?;
        let executable = info
            .as_dictionary()
            .and_then(|info| info.get("CFBundleExecutable"))
            .and_then(Value::as_string)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "bundle has no CFBundleExecutable",
                )
            })?;
        let mut components = Path::new(executable).components();
        if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "CFBundleExecutable must be a file name",
            ));
        }
        std::fs::create_dir_all(staging_root)?;
        let directory = tempfile::Builder::new()
            .prefix("polymer-guest-")
            .tempdir_in(staging_root)?;
        let bundle = directory.path().join(
            source
                .file_name()
                .ok_or_else(|| io::Error::other("bundle has no name"))?,
        );
        Self::run(Command::new("/bin/cp").arg("-Rc").arg(&source).arg(&bundle))?;
        // Quarantine must go before the first launch or signature assessment.
        Self::run(
            Command::new("/usr/bin/xattr")
                .args(["-dr", "com.apple.quarantine"])
                .arg(&bundle),
        )?;
        let entitlements = directory.path().join("entitlements.plist");
        Self::run(
            Command::new("/usr/bin/codesign")
                .args(["-d", "--entitlements"])
                .arg(&entitlements)
                .arg("--xml")
                .arg(&bundle),
        )?;
        let mut value = if entitlements.exists() && entitlements.metadata()?.len() != 0 {
            Value::from_file(&entitlements).map_err(io::Error::other)?
        } else {
            Value::Dictionary(Default::default())
        };
        let dictionary = value.as_dictionary_mut().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "entitlements are not a dictionary",
            )
        })?;
        for key in [
            "com.apple.security.cs.allow-dyld-environment-variables",
            "com.apple.security.cs.disable-library-validation",
        ] {
            dictionary.insert(key.into(), Value::Boolean(true));
        }
        value.to_file_xml(&entitlements).map_err(io::Error::other)?;
        Self::run(
            Command::new("/usr/bin/codesign")
                .args(["-f", "-s", "-", "--options", "runtime", "--entitlements"])
                .arg(&entitlements)
                .arg(&bundle),
        )?;
        Self::run(
            Command::new("/usr/bin/codesign")
                .args(["--verify", "--deep", "--strict"])
                .arg(&bundle),
        )?;
        let executable = bundle.join("Contents/MacOS").join(executable);
        if !executable.is_file()
            || !executable
                .canonicalize()?
                .starts_with(bundle.canonicalize()?)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bundle executable is missing or escapes the staged bundle",
            ));
        }
        Ok(Self {
            _directory: directory,
            bundle,
            executable,
        })
    }

    pub fn path(&self) -> &Path {
        &self.bundle
    }
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    fn run(command: &mut Command) -> io::Result<Output> {
        let output = command.output()?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "{} failed ({}): {}",
                command.get_program().to_string_lossy(),
                output.status,
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(output)
    }
}

impl polymer_vfs::ResourceFactory<dyn polymer_vfs::Volume> for StagedBundle {
    fn materialize(
        &self,
    ) -> polymer_vfs::VfsResult<polymer_vfs::Materialized<dyn polymer_vfs::Volume>> {
        polymer_vfs::HostDirectoryFactory::new(self.path().to_owned(), true).materialize()
    }
}
