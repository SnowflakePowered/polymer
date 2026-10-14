use std::{fmt::Display, path::Path, str::FromStr};

use serde::Deserialize;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned, little_endian::U32};

use crate::Error;

/// The trusted comment of a release signature, which binds it to one plugin version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrustedComment {
    pub id: String,
    pub version: semver::Version,
}

impl Display for TrustedComment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "id={} version={}", self.id, self.version)
    }
}

impl FromStr for TrustedComment {
    type Err = Error;

    fn from_str(comment: &str) -> Result<Self, Error> {
        let invalid = || {
            Error::Invalid(format!(
                "trusted comment '{comment}' is not 'id=… version=…'"
            ))
        };
        let (id, version) = comment
            .strip_prefix("id=")
            .and_then(|rest| rest.split_once(" version="))
            .ok_or_else(invalid)?;
        Ok(Self {
            id: id.to_owned(),
            version: semver::Version::parse(version).map_err(|_| invalid())?,
        })
    }
}

/// The identity a plugin declares in the `[plugin]` table of its `plugin.toml`.
#[derive(Clone, Debug, Deserialize)]
pub struct PluginHeader {
    pub id: String,
    pub version: semver::Version,
    pub class: String,
    pub api: semver::VersionReq,
}

impl PluginHeader {
    pub fn parse(manifest: &str) -> Result<Self, Error> {
        #[derive(Deserialize)]
        struct Manifest {
            plugin: PluginHeader,
        }
        Ok(toml::from_str::<Manifest>(manifest)?.plugin)
    }

    /// Reads the header from the `plugin.toml` at the root of a plugin archive.
    pub fn from_archive(archive: &[u8]) -> Result<Self, Error> {
        let reader = asar::AsarReader::new(archive, None)
            .map_err(|error| Error::Archive(error.to_string()))?;
        let manifest = reader
            .read(Path::new("plugin.toml"))
            .ok_or_else(|| Error::Archive("no plugin.toml at the archive root".to_owned()))?;
        Self::parse(
            std::str::from_utf8(manifest.data())
                .map_err(|_| Error::Archive("plugin.toml is not UTF-8".to_owned()))?,
        )
    }
}

/// Ends a signed archive, after the minisign signature it describes.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct Footer {
    length: U32,
    magic: [u8; 8],
}

/// A plugin archive with its minisign signature appended:
/// `[asar][signature][u32 signature length][POLYMSIG]`. Asar readers ignore the trailing bytes.
pub struct SignedArchive<'a> {
    archive: &'a [u8],
    signature: minisign_verify::Signature,
}

impl<'a> SignedArchive<'a> {
    const MAGIC: [u8; 8] = *b"POLYMSIG";

    /// Splits a signed archive into the asar and its signature.
    pub fn split(file: &'a [u8]) -> Result<Self, Error> {
        let unsigned = || Error::Archive("the archive is not signed".to_owned());
        let (rest, footer) = Footer::ref_from_suffix(file).map_err(|_| unsigned())?;
        if footer.magic != Self::MAGIC {
            return Err(unsigned());
        }
        let split = usize::try_from(footer.length.get())
            .ok()
            .and_then(|length| rest.len().checked_sub(length))
            .ok_or_else(unsigned)?;
        let (archive, signature) = rest.split_at(split);
        let signature = std::str::from_utf8(signature)
            .ok()
            .and_then(|signature| minisign_verify::Signature::decode(signature).ok())
            .ok_or_else(|| {
                Error::Archive("the appended signature is not a minisign signature".to_owned())
            })?;
        Ok(Self { archive, signature })
    }

    /// Appends a minisign signature to an unsigned archive.
    pub fn seal(archive: &[u8], signature: &str) -> Vec<u8> {
        let footer = Footer {
            length: U32::new(
                u32::try_from(signature.len()).expect("minisign signatures are small"),
            ),
            magic: Self::MAGIC,
        };
        [archive, signature.as_bytes(), footer.as_bytes()].concat()
    }

    /// The asar, without the signature.
    pub fn archive(&self) -> &'a [u8] {
        self.archive
    }

    /// The plugin and version the signature was made for.
    pub fn trusted_comment(&self) -> Result<TrustedComment, Error> {
        self.signature.trusted_comment().parse()
    }

    /// Checks that one of `keys` signed the archive, and that the archive's `plugin.toml`
    /// declares the plugin and version the signature names.
    pub fn verify(&self, keys: &[minisign_verify::PublicKey]) -> Result<PluginHeader, Error> {
        let comment = self.trusted_comment()?;
        let failure = |message: String| Error::Signature {
            id: comment.id.clone(),
            version: comment.version.to_string(),
            message,
        };
        if keys.is_empty() {
            return Err(failure(
                "the plugin index registers no keys for this plugin".to_owned(),
            ));
        }
        if !keys
            .iter()
            .any(|key| key.verify(self.archive, &self.signature, false).is_ok())
        {
            return Err(failure("no registered key signed this archive".to_owned()));
        }
        let header = PluginHeader::from_archive(self.archive)?;
        if header.id != comment.id || header.version != comment.version {
            return Err(failure(format!(
                "the archive declares {} {}",
                header.id, header.version
            )));
        }
        Ok(header)
    }
}
