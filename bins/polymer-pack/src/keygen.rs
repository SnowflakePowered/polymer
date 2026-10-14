use std::{fs, path::PathBuf};

#[derive(clap::Args)]
pub struct Keygen {
    /// Where the secret key is written. The public key is written next to it with a `.pub`
    /// extension.
    #[arg(long, default_value = "polymer.key")]
    out: PathBuf,
}

impl Keygen {
    pub fn run(self) -> crate::Result<()> {
        let public = self.out.with_extension("pub");
        for path in [&self.out, &public] {
            if path.exists() {
                return Err(format!("{} already exists", path.display()).into());
            }
        }
        let pair = minisign::KeyPair::generate_unencrypted_keypair()?;
        fs::write(&self.out, pair.sk.to_box(None)?.into_string())?;
        fs::write(&public, pair.pk.to_box()?.into_string())?;
        eprintln!(
            "Wrote {} and {}. Add this public key to the repository's polymer-index entry:",
            self.out.display(),
            public.display()
        );
        println!("{}", pair.pk.to_base64());
        Ok(())
    }
}
