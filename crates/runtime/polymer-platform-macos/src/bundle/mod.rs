//! Prepare a private emulator copy for launch-time dyld injection.

mod cache;

use plist::Value;
use std::{
    io,
    path::{Component, Path, PathBuf},
    process::{Command, Output},
};

/// References a cached, locally re-signed bundle.
/// Stage before adding the bundle to the mandatory guest filesystem; this is
/// preparation, not a launch API or a way to bypass the VFS.
pub struct StagedBundle {
    bundle: PathBuf,
    executable: PathBuf,
}

impl StagedBundle {
    /// Reuses a signed copy while the source contents and signing policy match.
    /// Never modifies the source bundle; cached versions survive guest teardown.
    pub fn prepare(source: &Path, staging_root: &Path) -> io::Result<Self> {
        cache::BundleCache::prepare(source, staging_root)
    }

    /// The per-user persistent cache shared by GUI and debug-viewer launches.
    pub fn cache_root() -> io::Result<PathBuf> {
        std::env::home_dir()
            .map(|home| home.join("Library/Caches/gg.polymer/signed-bundles"))
            .ok_or_else(|| io::Error::other("cannot locate the user's cache directory"))
    }

    fn open(bundle: PathBuf) -> io::Result<Self> {
        let info =
            Value::from_file(bundle.join("Contents/Info.plist")).map_err(io::Error::other)?;
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
        Ok(Self { bundle, executable })
    }

    fn sign(bundle: &Path, directory: &Path) -> io::Result<()> {
        // Quarantine must go before the first launch or signature assessment.
        Self::run(
            Command::new("/usr/bin/xattr")
                .args(["-dr", "com.apple.quarantine"])
                .arg(&bundle),
        )?;
        let entitlements = directory.join("entitlements.plist");
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
        Ok(())
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
    fn materialize_in(
        &self,
        scope: &mut polymer_vfs::MaterializationScope,
    ) -> polymer_vfs::VfsResult<std::sync::Arc<dyn polymer_vfs::Volume>> {
        scope.build(&polymer_vfs::HostDirectoryFactory::new(
            self.path().to_owned(),
            true,
        ))
    }
}

/// Materializes an injection-ready bundle only when the launch VFS is prepared.
pub struct BundleFactory {
    source: PathBuf,
}
impl BundleFactory {
    pub fn new(source: PathBuf) -> Self {
        Self { source }
    }
}
impl polymer_vfs::ResourceFactory<dyn polymer_vfs::Volume> for BundleFactory {
    fn materialize_in(
        &self,
        scope: &mut polymer_vfs::MaterializationScope,
    ) -> polymer_vfs::VfsResult<std::sync::Arc<dyn polymer_vfs::Volume>> {
        let staged = StagedBundle::prepare(&self.source, &StagedBundle::cache_root()?)?;
        let factory = polymer_vfs::PackageDirectoryFactory::new(staged.path().to_owned())
            .with_internal_links();
        let backend = scope.build(&factory)?;
        scope.push(std::sync::Arc::new(BundleLease { _staged: staged }));
        Ok(backend)
    }
}
struct BundleLease {
    _staged: StagedBundle,
}
impl polymer_vfs::ResourceFinalizer for BundleLease {
    fn finalize(&self, _reason: polymer_vfs::FinalizationReason) -> polymer_vfs::VfsResult<()> {
        Ok(())
    }
}
