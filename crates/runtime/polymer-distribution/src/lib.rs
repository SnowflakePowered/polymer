//! Formats for distributing plugins: a repository's `Polymer.toml`, its entry in the
//! polymer-index repository, and the signatures that tie a release to its archive.

mod index;
mod listing;
mod signature;

pub use index::{Index, IndexEntry};
pub use listing::{PluginListing, Release, RepositoryManifest};
pub use signature::{PluginHeader, SignedArchive, TrustedComment};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Toml(#[from] toml::de::Error),
    #[error("invalid plugin archive: {0}")]
    Archive(String),
    #[error("invalid signature for {id} {version}: {message}")]
    Signature {
        id: String,
        version: String,
        message: String,
    },
    #[error("{0}")]
    Invalid(String),
}
