use super::*;

pub(in crate::storage::git) struct ArtifactTree<'r>(pub(super) gix::Tree<'r>);

impl ArtifactTree<'_> {
    pub(super) fn value(&self, path: &str) -> Result<Option<Vec<u8>>, Error> {
        let Some(entry) = self
            .0
            .lookup_entry_by_path(path)
            .map_err(|error| Error::git("lookup artifact value", error))?
        else {
            return Ok(None);
        };
        self.0
            .repo
            .find_blob(entry.oid())
            .map(|blob| Some(blob.data.clone()))
            .map_err(|error| Error::git("read artifact value", error))
    }

    fn files(&self) -> Result<Option<gix::Tree<'_>>, Error> {
        let Some(files) = self
            .0
            .lookup_entry_by_path("files")
            .map_err(|error| Error::git("lookup artifact files", error))?
        else {
            return Ok(None);
        };
        self.0
            .repo
            .find_tree(files.oid())
            .map(Some)
            .map_err(|error| Error::git("read artifact files", error))
    }

    /// The artifact's embedded icons, in the order of their supported formats.
    fn icons(&self) -> Result<Vec<EmbeddedIcon>, Error> {
        let mut icons = Vec::new();
        for name in EmbeddedIcon::FILES {
            if let Some(bytes) = self.value(name)? {
                icons.extend(EmbeddedIcon::from_file(name, bytes));
            }
        }
        Ok(icons)
    }

    pub(in crate::storage::git) fn summary(&self) -> Result<ArtifactSummary, Error> {
        let text = |path: &str| -> Result<String, Error> {
            String::from_utf8(
                self.value(path)?
                    .ok_or_else(|| Error::HistoryConflict(format!("artifact has no `{path}`")))?,
            )
            .map_err(|error| Error::git("decode artifact value", error))
        };
        let depot_type = text("depot-type")?
            .parse::<u8>()
            .map_err(|error| Error::git("parse artifact type", error))?;
        Ok(ArtifactSummary {
            id: text("id")?,
            depot_type: DepotType::try_from(depot_type).map_err(|value| {
                Error::HistoryConflict(format!("invalid artifact type {value}"))
            })?,
            label: text("label")?,
            version: self
                .value("version")?
                .map(String::from_utf8)
                .transpose()
                .map_err(|error| Error::git("decode artifact version", error))?,
            byte_length: text("byte-length")?
                .parse::<u64>()
                .map_err(|error| Error::git("parse artifact size", error))?,
            installed: text("installed")?
                .parse::<Timestamp>()
                .map_err(|error| Error::git("parse artifact install time", error))?,
            file_count: self.files()?.map_or(0, |files| files.iter().count()),
            icons: self.icons()?.iter().map(EmbeddedIcon::data_url).collect(),
            entrypoint: self
                .value("root")?
                .zip(self.value("entrypoint")?)
                .map(|(root, path)| {
                    let (root, path) = (String::from_utf8(root)?, String::from_utf8(path)?);
                    let package = match self.value("package") {
                        Ok(Some(package)) => String::from_utf8(package)?,
                        _ => format!("{root}/{path}"),
                    };
                    Ok::<_, std::string::FromUtf8Error>(ArtifactEntrypoint {
                        root,
                        path,
                        package,
                    })
                })
                .transpose()
                .map_err(|error| Error::git("decode artifact entrypoint", error))?,
        })
    }

    pub(in crate::storage::git) fn record(&self) -> Result<ArtifactRecord, Error> {
        let ArtifactSummary {
            id,
            depot_type,
            label,
            version,
            byte_length,
            installed,
            entrypoint,
            ..
        } = self.summary()?;
        let icon_path = self
            .value("icon-path")?
            .map(String::from_utf8)
            .transpose()
            .map_err(|error| Error::git("decode artifact icon path", error))?
            .map(PathBuf::from);
        let mut manifest = Vec::new();
        for file in self.files()?.iter().flat_map(|files| files.iter()) {
            let file = file.map_err(|error| Error::git("decode artifact file", error))?;
            let file = ArtifactTree(
                self.0
                    .repo
                    .find_tree(file.oid())
                    .map_err(|error| Error::git("read artifact file", error))?,
            );
            let file_value = |name: &str| -> Result<Vec<u8>, Error> {
                file.value(name)?
                    .ok_or_else(|| Error::HistoryConflict(format!("artifact file has no `{name}`")))
            };
            let path = PathBuf::from(
                String::from_utf8(file_value("path")?)
                    .map_err(|error| Error::git("decode artifact file path", error))?,
            );
            let blake3: [u8; 32] = file_value("blake3")?.try_into().map_err(|_| {
                Error::HistoryConflict("artifact digest has the wrong length".to_owned())
            })?;
            let byte_length = String::from_utf8(file_value("byte-length")?)
                .map_err(|error| Error::git("decode artifact file size", error))?
                .parse::<u64>()
                .map_err(|error| Error::git("parse artifact file size", error))?;
            manifest.push(ArtifactFileRecord {
                path,
                blake3,
                byte_length,
            });
        }
        Ok(ArtifactRecord {
            id,
            depot_type,
            manifest,
            label,
            version,
            icon_path,
            icons: self.icons()?,
            byte_length,
            installed,
            entrypoint,
        })
    }
}
