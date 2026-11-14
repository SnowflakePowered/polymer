use std::path::{Path, PathBuf};

use polymer_distribution::{IndexEntry, PluginHeader, RepositoryManifest, SignedArchive};

#[derive(clap::Args)]
pub struct Verify {
    #[arg(long, default_value = "Polymer.toml")]
    manifest: PathBuf,
    /// The repository's entry in polymer-index, whose keys sign its releases.
    #[arg(long)]
    index: Option<PathBuf>,
    /// Downloads every release that is not yanked and checks its signature and contents.
    #[arg(long, requires = "index")]
    fetch: bool,
}

impl Verify {
    pub fn run(self) -> crate::Result<()> {
        let manifest = RepositoryManifest::read(&self.manifest)?;
        let base = self.manifest.parent().unwrap_or(Path::new(""));
        let entry = self.index.as_deref().map(IndexEntry::read).transpose()?;
        for listing in &manifest.plugin {
            let header = PluginHeader::parse(&std::fs::read_to_string(
                base.join(&listing.path).join("plugin.toml"),
            )?)?;
            if header.id != listing.id || header.class != listing.class {
                return Err(format!(
                    "{} declares {} plugin '{}', not {} plugin '{}'",
                    listing.path.display(),
                    header.class,
                    header.id,
                    listing.class,
                    listing.id
                )
                .into());
            }
            if entry
                .as_ref()
                .is_some_and(|entry| !entry.publishes(&listing.id))
            {
                return Err(
                    format!("the index entry may not publish plugin '{}'", listing.id).into(),
                );
            }
            println!("{}: {} release(s)", listing.id, listing.release.len());
        }
        if let (true, Some(entry)) = (self.fetch, &entry) {
            Self::releases(&manifest, entry)?;
        }
        Ok(())
    }

    /// Downloads every release that is not yanked and checks its signature against the keys of
    /// `entry`, and its contents against the listing.
    pub fn releases(manifest: &RepositoryManifest, entry: &IndexEntry) -> crate::Result<()> {
        let keys = entry.public_keys();
        let client = reqwest::blocking::Client::new();
        for listing in &manifest.plugin {
            for release in listing.release.iter().filter(|release| !release.yanked) {
                let file = client
                    .get(&release.url)
                    .send()?
                    .error_for_status()?
                    .bytes()?;
                listing.matches(release, &SignedArchive::split(&file)?.verify(&keys)?)?;
                println!(
                    "{} {}: signature and contents verified",
                    listing.id, release.version
                );
            }
        }
        Ok(())
    }
}
