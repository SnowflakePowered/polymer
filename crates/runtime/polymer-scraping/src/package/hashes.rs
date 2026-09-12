use super::Package;
use md5::Md5;
use polymer_library::MetadataInput;
use sha1::{Digest, Sha1};
use std::{fs::File, io::Read, path::Path};

/// Dump hashes describe the file bytes, independently of Gumshoe's normalized
/// executable digest used as the library's binary identity.
#[derive(Default)]
pub(super) struct FileDigests {
    sha1: Sha1,
    md5: Md5,
    crc32: crc32fast::Hasher,
    size: u64,
}

impl FileDigests {
    pub(super) fn metadata(path: &Path) -> Result<Vec<MetadataInput>, crate::ScrapingError> {
        if !path.is_file() || Self::is_container(path) {
            return Ok(Vec::new());
        }
        Self::read(path).map_err(|source| crate::ScrapingError::Open {
            path: path.to_owned(),
            source,
        })
    }

    fn is_container(path: &Path) -> bool {
        let extension = path
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase();
        // Container digests cannot identify a decompressed dump or a disc track.
        matches!(
            extension.as_str(),
            "zip"
                | "7z"
                | "rar"
                | "gz"
                | "xz"
                | "zst"
                | "chd"
                | "cso"
                | "zso"
                | "rvz"
                | "wia"
                | "gcz"
                | "wbfs"
                | "nkit"
                | "pkg"
                | "cia"
                | "nsp"
                | "nsz"
                | "xcz"
                | "m3u"
                | "m3u8"
                | "gdi"
                | "ccd"
                | "mds"
        )
    }

    fn read(path: &Path) -> Result<Vec<MetadataInput>, std::io::Error> {
        let mut file = File::open(path)?;
        let mut digest = Self::default();
        let mut buffer = vec![0; 1024 * 1024];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            let bytes = &buffer[..count];
            digest.sha1.update(bytes);
            digest.md5.update(bytes);
            digest.crc32.update(bytes);
            digest.size += count as u64;
        }
        let sha1 = format!("{:x}", digest.sha1.finalize());
        // The same source ties all digests and size to one file even when scrape
        // input combines metadata from a root game and a child binary.
        let source = format!("Raw file SHA-1 {sha1}");
        Ok([
            ("package.hash.sha1", sha1),
            ("package.hash.md5", format!("{:x}", digest.md5.finalize())),
            (
                "package.hash.crc32",
                format!("{:08x}", digest.crc32.finalize()),
            ),
            ("package.size", digest.size.to_string()),
        ]
        .into_iter()
        .map(|(key, value)| Package::scalar(key, &source, value))
        .collect())
    }
}
