use std::{fs, path::PathBuf};

use polymer_distribution::{PluginHeader, SignedArchive, TrustedComment};

#[derive(clap::Args)]
pub struct Sign {
    /// Minisign secret key. Defaults to the POLYMER_SIGNING_KEY environment variable,
    /// which holds the key itself.
    #[arg(long)]
    key: Option<PathBuf>,
    /// The packed archive, signed in place.
    archive: PathBuf,
}

impl Sign {
    pub fn run(self) -> crate::Result<()> {
        let archive = fs::read(&self.archive)?;
        if SignedArchive::split(&archive).is_ok() {
            return Err(format!("{} is already signed", self.archive.display()).into());
        }
        let header = PluginHeader::from_archive(&archive)?;
        let key = match &self.key {
            Some(path) => fs::read_to_string(path)?,
            None => std::env::var("POLYMER_SIGNING_KEY")
                .map_err(|_| "pass --key or set POLYMER_SIGNING_KEY")?,
        };
        let secret = minisign::SecretKeyBox::from_string(&key)?.into_unencrypted_secret_key()?;
        let comment = TrustedComment {
            id: header.id.clone(),
            version: header.version.clone(),
        };
        let signature = minisign::sign(
            None,
            &secret,
            archive.as_slice(),
            Some(&comment.to_string()),
            None,
        )?
        .into_string();
        fs::write(&self.archive, SignedArchive::seal(&archive, &signature))?;
        println!("Signed {} {}", header.id, header.version);
        Ok(())
    }
}
