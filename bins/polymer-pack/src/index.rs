use std::{collections::BTreeMap, path::PathBuf};

use polymer_distribution::{Index, RepositoryManifest};

use crate::verify::Verify;

#[derive(clap::Args)]
pub struct CheckIndex {
    /// A checkout of the polymer-index repository.
    #[arg(default_value = ".")]
    root: PathBuf,
    /// Also downloads every registered repository's Polymer.toml and checks its releases.
    #[arg(long)]
    fetch: bool,
}

impl CheckIndex {
    pub fn run(self) -> crate::Result<()> {
        let index = Index::read(&self.root)?;
        if let Some((path, error)) = index.rejected.first() {
            return Err(format!("{}: {error}", path.display()).into());
        }
        let mut claimed = BTreeMap::new();
        for entry in &index.entries {
            for scope in &entry.scopes {
                if let Some(other) = claimed.insert(scope, &entry.repo) {
                    return Err(format!(
                        "scope '{scope}' is claimed by both {other} and {}",
                        entry.repo
                    )
                    .into());
                }
            }
            let expected = self.root.join("repos").join(format!("{}.toml", entry.repo));
            if !expected.is_file() {
                return Err(format!(
                    "{} must be registered in {}",
                    entry.repo,
                    expected.display()
                )
                .into());
            }
            println!("{}: ok", entry.repo);
        }
        if !self.fetch {
            return Ok(());
        }
        let client = reqwest::blocking::Client::new();
        for entry in &index.entries {
            let url = format!(
                "https://raw.githubusercontent.com/{}/{}/Polymer.toml",
                entry.repo, entry.reference
            );
            let text = client.get(&url).send()?.error_for_status()?.text()?;
            let manifest = RepositoryManifest::parse(&text)?;
            if let Some(listing) = manifest
                .plugin
                .iter()
                .find(|listing| !entry.publishes(&listing.id))
            {
                return Err(
                    format!("{} may not publish plugin '{}'", entry.repo, listing.id).into(),
                );
            }
            Verify::releases(&manifest, entry)?;
        }
        Ok(())
    }
}
