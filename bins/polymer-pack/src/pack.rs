use std::{
    fs,
    path::{Path, PathBuf},
};

use polymer_distribution::RepositoryManifest;
use polymer_pluginmanifest::{AnyPluginManifest, PackageSource, PluginManifest, ThemeBlock};

#[derive(clap::Args)]
pub struct Pack {
    /// ID of the plugin in Polymer.toml.
    #[arg(long)]
    id: String,
    #[arg(long, default_value = "Polymer.toml")]
    manifest: PathBuf,
    /// Directory the archive is written to.
    #[arg(long, default_value = ".")]
    out: PathBuf,
}

impl Pack {
    const SKIPPED: [&str; 2] = ["node_modules", ".git"];

    pub fn run(self) -> crate::Result<()> {
        let manifest = RepositoryManifest::read(&self.manifest)?;
        let listing = manifest.listing(&self.id).ok_or_else(|| {
            format!(
                "{} does not list plugin '{}'",
                self.manifest.display(),
                self.id
            )
        })?;
        let root = self
            .manifest
            .parent()
            .unwrap_or(Path::new(""))
            .join(&listing.path);
        let plugin = AnyPluginManifest::load(PackageSource::directory(&root)?)?;
        if plugin.id() != self.id {
            return Err(format!(
                "{} declares plugin '{}', not '{}'",
                root.display(),
                plugin.id(),
                self.id
            )
            .into());
        }
        let version = semver::Version::parse(plugin.version())?;
        let mut files = match &plugin {
            AnyPluginManifest::Theme(theme) if theme.theme_root().is_some() => {
                Self::theme_files(&root, theme)?
            }
            _ => Self::files(&root, &root)?,
        };
        files.sort();
        let mut writer = asar::AsarWriter::new();
        for relative in &files {
            writer.write_file(relative, fs::read(root.join(relative))?, false)?;
        }
        fs::create_dir_all(&self.out)?;
        let name = self.id.trim_start_matches('@').replace('/', "-");
        let archive = self.out.join(format!("{name}-{version}.asar"));
        writer.finalize(fs::File::create(&archive)?)?;
        println!("{}", archive.display());
        Ok(())
    }

    fn theme_files(root: &Path, theme: &PluginManifest<ThemeBlock>) -> crate::Result<Vec<PathBuf>> {
        let mut files = vec![PathBuf::from("plugin.toml")];
        for directory in [theme.theme_root(), theme.assets()].into_iter().flatten() {
            let directory = root.join(directory);
            if !directory.is_dir() {
                return Err(format!(
                    "{} does not exist; build the theme first",
                    directory.display()
                )
                .into());
            }
            files.extend(Self::files(root, &directory)?);
        }
        Ok(files)
    }

    fn files(root: &Path, directory: &Path) -> crate::Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name();
            let kind = entry.file_type()?;
            if Self::SKIPPED.iter().any(|skipped| name == *skipped)
                || (directory == root && name == "bin")
                || kind.is_symlink()
            {
                continue;
            }
            if kind.is_dir() {
                files.extend(Self::files(root, &path)?);
            } else {
                files.push(path.strip_prefix(root)?.to_owned());
            }
        }
        Ok(files)
    }
}
