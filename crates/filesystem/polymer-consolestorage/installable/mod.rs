use crate::nand::VirtualNand;
use std::path::{Path, PathBuf};

pub mod artifacts;
mod directory;
mod installation;
#[cfg(test)]
mod tests;
mod writer;
pub(crate) use writer::StorageWriter;

pub use directory::DirectoryInstall;
pub use installation::{Installable, Installation};

/// Discriminants are persisted in the artifact database, so they are fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
#[repr(u8)]
pub enum DepotType {
    /// System file.
    System = 0,
    /// A game or application.
    App = 1,
    /// DLC for a game or application.
    AppContent = 2,
    /// An update for a game or application.
    AppUpdate = 3,
    /// Ticket for a game or application.
    AppTicket = 4,
}

impl From<DepotType> for u8 {
    fn from(depot_type: DepotType) -> u8 {
        depot_type as u8
    }
}

impl TryFrom<u8> for DepotType {
    /// The unrecognized discriminant.
    type Error = u8;

    fn try_from(id: u8) -> Result<Self, u8> {
        match id {
            0 => Ok(DepotType::System),
            1 => Ok(DepotType::App),
            2 => Ok(DepotType::AppContent),
            3 => Ok(DepotType::AppUpdate),
            4 => Ok(DepotType::AppTicket),
            other => Err(other),
        }
    }
}

pub trait Depot: Sized {
    /// Parse this depot type from a path. Invalid input must fail construction.
    fn open(path: &Path) -> Result<Self, crate::Error>;

    /// Check whether the target NAND can accept this constructed depot.
    fn satisfies<N: VirtualNand + ?Sized>(&self, nand: &N) -> bool;

    /// Validate the contents of the depot.
    ///
    /// Returns the names of the contents that don't validate.
    fn validate_contents(&self) -> Vec<String>;

    fn depot_type(&self) -> DepotType;

    /// Install a depot, exposing progress through `next().await` and its result
    /// through `finish().await`. Calling `finish` also drives any remaining work.
    #[must_use]
    fn install<'a, N: VirtualNand + ?Sized>(self, nand: &'a N) -> Installation<'a>
    where
        Self: 'a;
}

#[derive(Clone)]
pub enum IconHint {
    /// No custom icon.
    Builtin(DepotType),
    /// The icon is a file in the installed artifact.
    File(PathBuf),
}

#[derive(Clone)]
pub struct DepotFile {
    path: PathBuf,
    blake3: [u8; 32],
    byte_length: u64,
}

impl DepotFile {
    pub fn new(path: PathBuf, blake3: [u8; 32], byte_length: u64) -> Self {
        Self {
            path,
            blake3,
            byte_length,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn blake3(&self) -> &[u8; 32] {
        &self.blake3
    }

    pub fn byte_length(&self) -> u64 {
        self.byte_length
    }
}
