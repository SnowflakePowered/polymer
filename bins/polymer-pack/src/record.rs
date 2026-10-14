use std::{fs, path::PathBuf};

use polymer_distribution::{PluginHeader, RepositoryManifest, SignedArchive};

#[derive(clap::Args)]
pub struct Record {
    /// ID of the plugin in Polymer.toml.
    #[arg(long)]
    id: String,
    /// Where the archive is published.
    #[arg(long)]
    url: String,
    #[arg(long, default_value = "Polymer.toml")]
    manifest: PathBuf,
    /// The signed archive.
    archive: PathBuf,
}

impl Record {
    pub fn run(self) -> crate::Result<()> {
        let file = fs::read(&self.archive)?;
        let header = PluginHeader::from_archive(SignedArchive::split(&file)?.archive())?;
        if header.id != self.id {
            return Err(format!(
                "{} holds plugin '{}', not '{}'",
                self.archive.display(),
                header.id,
                self.id
            )
            .into());
        }
        let text = fs::read_to_string(&self.manifest)?;
        let manifest = RepositoryManifest::parse(&text)?;
        let position = manifest
            .plugin
            .iter()
            .position(|listing| listing.id == self.id)
            .ok_or_else(|| {
                format!(
                    "{} does not list plugin '{}'",
                    self.manifest.display(),
                    self.id
                )
            })?;
        let listing = &manifest.plugin[position];
        if header.class != listing.class {
            return Err(format!(
                "{} is a {} plugin, but {} lists it as {}",
                self.id,
                header.class,
                self.manifest.display(),
                listing.class
            )
            .into());
        }
        if listing
            .release
            .iter()
            .any(|release| release.version == header.version)
        {
            return Err(format!("{} {} is already recorded", self.id, header.version).into());
        }
        let updated = self.insert(&text, position, &header);
        let recorded = RepositoryManifest::parse(&updated)?;
        if !recorded.listing(&self.id).is_some_and(|listing| {
            listing
                .release
                .iter()
                .any(|release| release.version == header.version)
        }) {
            return Err("the release was not recorded under its plugin".into());
        }
        fs::write(&self.manifest, updated)?;
        println!("Recorded {} {}", self.id, header.version);
        Ok(())
    }

    /// Appends a release table to the end of the `position`th `[[plugin]]` section.
    fn insert(&self, text: &str, position: usize, header: &PluginHeader) -> String {
        let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let quoted = |value: String| toml::Value::String(value).to_string();
        let release = [
            "[[plugin.release]]".to_owned(),
            format!("version = {}", quoted(header.version.to_string())),
            format!("api = {}", quoted(header.api.to_string())),
            format!("url = {}", quoted(self.url.clone())),
            "yanked = false".to_owned(),
        ]
        .join(nl);
        let mut offset = 0;
        let mut sections = Vec::new();
        for line in text.split_inclusive('\n') {
            if line.trim() == "[[plugin]]" {
                sections.push(offset);
            }
            offset += line.len();
        }
        let end = sections.get(position + 1).copied().unwrap_or(text.len());
        let (before, after) = text.split_at(end);
        let separator = if after.is_empty() { "" } else { nl };
        format!(
            "{}{nl}{nl}{release}{nl}{separator}{after}",
            before.trim_end()
        )
    }
}
