use super::Package;
use polymer_library::{GameCatalog, GameCatalogError, IdentifiedLaunchable, Launchable};
use polymer_pluginhost::digest::{DigestAlgorithm, Digestible, PackageBinary};

impl Digestible for Package {
    fn digest(&self, algorithm: DigestAlgorithm) -> std::io::Result<String> {
        self.uri.digest(algorithm)
    }
}

impl Package {
    /// Library identity and battery-save naming require BLAKE3. Merely identifying
    /// or exposing a package to JS does not compute it.
    pub(super) fn ensure_binary_identity(&mut self) -> Result<[u8; 32], std::io::Error> {
        if self.binary_digest.is_empty() {
            let digest =
                PackageBinary(&self.uri.package_path()?).digest(DigestAlgorithm::Blake3)?;
            if let Some(manifest) = &self.manifest {
                crate::ingest::CuePreparation::validate_files(manifest)
                    .map_err(std::io::Error::other)?;
            }
            self.binary_digest = digest;
        }
        blake3::Hash::from_hex(&self.binary_digest)
            .map(|hash| *hash.as_bytes())
            .map_err(std::io::Error::other)
    }

    pub async fn import(
        mut self,
        catalog: &GameCatalog,
    ) -> Result<(Launchable, bool), GameCatalogError> {
        let seed = self.import.take().ok_or_else(|| {
            GameCatalogError::InvalidData(
                "a Package without import metadata cannot be imported; identify it from its path first".to_owned(),
            )
        })?;
        let (package, binary_blake3) = tokio::task::spawn_blocking(move || {
            let digest = self.ensure_binary_identity()?;
            Ok::<_, std::io::Error>((self, digest))
        })
        .await
        .map_err(|error| GameCatalogError::InvalidData(error.to_string()))?
        .map_err(|error| GameCatalogError::InvalidData(error.to_string()))?;
        Box::pin(catalog.import_package(
            IdentifiedLaunchable {
                manifest: package.manifest,
                path: package.uri,
                entrypoint: seed.entrypoint,
                package: seed.package,
                binary_blake3,
                serial: package.serial,
                title_id: package.title_id,
                disc_number: package.disc_number,
                requires_license: package.requires_license,
                parent_binary_id: seed.parent_binary_id,
                platform: package.platform,
            },
            vec![
                ("host:gumshoe".to_owned(), seed.gumshoe),
                ("host:shiratsu".to_owned(), seed.filename),
                // Clear legacy eager hashes when reimporting an existing game.
                ("host:raw-file".to_owned(), Vec::new()),
            ],
        ))
        .await
    }
}
