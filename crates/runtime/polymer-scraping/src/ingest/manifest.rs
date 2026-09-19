use super::CuePreparation;
use crate::ScrapingError;
use polymer_library::{PackageFile, PackageManifest};
use serde::{Deserialize, Serialize};
use std::{path::Path, time::UNIX_EPOCH};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchBundle {
    pub revision: String,
    pub entrypoint: String,
    pub files: Vec<LaunchFile>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchFile {
    pub name: String,
    pub path: Option<std::path::PathBuf>,
    pub text: Option<String>,
}

impl CuePreparation {
    pub(crate) fn capture(
        path: &Path,
        package: &dyn gumshoe::Package,
    ) -> Result<Option<PackageManifest>, ScrapingError> {
        if package.package().is_some() {
            return Ok(None);
        }
        let mut members = package.package_files();
        let Some(descriptor) = members.next().transpose()? else {
            return Ok(None);
        };
        let Some(name) = descriptor.filename() else {
            return Ok(None);
        };
        let descriptor_path = if path.is_dir() {
            path.join(name)
        } else {
            path.to_owned()
        };
        let descriptor_file = Self::file(name, &descriptor_path)?;
        let bytes = std::fs::read(&descriptor_path)?;
        // The public stream API exposes digests, so verify the captured bytes against
        // Gumshoe's already-open descriptor rather than silently accepting an edit.
        let digest = descriptor.digest::<blake3::Hasher>(None)?;
        if digest.as_slice() != blake3::hash(&bytes).as_bytes() {
            return Err(ScrapingError::InvalidMedia(
                "CUE changed during identification; import it again".into(),
            ));
        }
        let base = descriptor_path.parent().unwrap_or(Path::new("."));
        let mut files = Vec::new();
        for member in members {
            let member = member?;
            let name = member.filename().ok_or_else(|| {
                ScrapingError::InvalidMedia("package member has no filename".into())
            })?;
            files.push(Self::file(name, &base.join(name.replace('\\', "/")))?);
        }
        let cue = String::from_utf8(bytes)
            .map_err(|error| ScrapingError::InvalidMedia(error.to_string()))?;
        let mut manifest = PackageManifest {
            version: 1,
            revision: String::new(),
            descriptor: descriptor_file,
            cue,
            files,
            template: None,
            error: None,
        };
        manifest.revision = blake3::hash(
            &serde_json::to_vec(&manifest)
                .map_err(|error| ScrapingError::InvalidMedia(error.to_string()))?,
        )
        .to_hex()
        .to_string();
        if let Err(error) = Self::finalize(&mut manifest) {
            manifest.error = Some(error.to_string());
        }
        Ok(Some(manifest))
    }

    fn file(name: &str, path: &Path) -> Result<PackageFile, ScrapingError> {
        let path = path.canonicalize()?;
        let metadata = std::fs::metadata(&path)?;
        if !metadata.is_file() {
            return Err(ScrapingError::InvalidMedia(format!(
                "{} is not a regular file",
                path.display()
            )));
        }
        let modified_ns = metadata
            .modified()?
            .duration_since(UNIX_EPOCH)
            .map_err(|error| ScrapingError::InvalidMedia(error.to_string()))?
            .as_nanos()
            .to_string();
        Ok(PackageFile {
            name: name.to_owned(),
            path,
            size: metadata.len(),
            modified_ns,
        })
    }

    pub(crate) fn validate_files(manifest: &PackageManifest) -> Result<(), ScrapingError> {
        for file in std::iter::once(&manifest.descriptor).chain(&manifest.files) {
            let current = Self::file(&file.name, &file.path).map_err(|error| {
                ScrapingError::InvalidMedia(format!(
                    "package file {} is unavailable: {error}",
                    file.path.display()
                ))
            })?;
            if &current != file {
                return Err(ScrapingError::InvalidMedia(format!(
                    "package file {} changed; import the package again",
                    file.path.display()
                )));
            }
        }
        Ok(())
    }

    pub(crate) fn bundle(manifest: &PackageManifest) -> Result<LaunchBundle, ScrapingError> {
        if manifest.version != 1 {
            return Err(ScrapingError::InvalidMedia(
                "unsupported package manifest version".into(),
            ));
        }
        Self::validate_files(manifest)?;
        let template = manifest.template.as_ref().ok_or_else(|| {
            ScrapingError::InvalidMedia(
                manifest
                    .error
                    .clone()
                    .unwrap_or_else(|| "package preparation is pending".into()),
            )
        })?;
        let names: Vec<_> = manifest
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| {
                let extension = Path::new(&file.name)
                    .extension()
                    .and_then(|s| s.to_str())
                    .filter(|s| s.bytes().all(|b| b.is_ascii_alphanumeric()))
                    .unwrap_or("bin");
                format!("file-{index:04}.{extension}")
            })
            .collect();
        let mut text = String::new();
        for part in &template.parts {
            match part {
                polymer_library::CuePart::Text(value) => text.push_str(value),
                polymer_library::CuePart::File(index) => {
                    let name = names.get(*index).ok_or_else(|| {
                        ScrapingError::InvalidMedia("invalid CUE member slot".into())
                    })?;
                    text.push('"');
                    text.push_str(name);
                    text.push('"');
                }
            }
        }
        let mut files: Vec<_> = manifest
            .files
            .iter()
            .zip(names)
            .map(|(file, name)| LaunchFile {
                name,
                path: Some(file.path.clone()),
                text: None,
            })
            .collect();
        files.push(LaunchFile {
            name: "disc.cue".into(),
            path: None,
            text: Some(text),
        });
        Ok(LaunchBundle {
            revision: manifest.revision.clone(),
            entrypoint: "disc.cue".into(),
            files,
        })
    }
}
