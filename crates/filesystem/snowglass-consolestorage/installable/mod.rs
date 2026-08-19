use std::path::{Path, PathBuf};
use crate::Error;
use crate::nand::VirtualNand;
use futures::prelude::Stream;
use artifacts::Artifact;

mod directory;
pub mod artifacts;

pub use directory::DirectoryInstall;

/// Discriminants are persisted in the artifact database, so they are fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DepotType {
    App = 0,
    AppContent = 1,
    AppUpdate = 2,
    System = 3,
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
            0 => Ok(DepotType::App),
            1 => Ok(DepotType::AppContent),
            2 => Ok(DepotType::AppUpdate),
            3 => Ok(DepotType::System),
            other => Err(other),
        }
    }
}

pub trait Depot {
    fn satisfies<N: VirtualNand>(&self, nand: &N) -> bool;
    
    /// Validate the contents of the depot. 
    /// 
    /// Returns the names of the contents that don't validate.
    fn validate_contents(&self) -> Vec<String>;
    
    fn depot_type(&self) -> DepotType;

    /// Install the depot to the nand `N`.
    ///
    /// Note that this does not take a mutable reference to
    /// `N` otherwise a borrow would need to be held for the length
    /// of `N`. Instead, the virtual nand should have interior
    /// mutability.
    ///
    /// This function must return a future and a stream.
    ///
    /// The future returns the resultant `Artifact` once installation is complete.
    /// Polling this future **does not** drive the installation process and will hang forever, until
    /// the stream is polled, which drives the installation and must be polled to completion.
    ///
    /// If the stream encounters an error, it will terminate. Only after, will the future return
    /// the same error as the last poll of the stream returned.
    #[must_use]
    fn install<'a, N: VirtualNand>(&'a self, nand: &'a N) -> (Box<dyn Future<Output=Result<Artifact, Error>>>,
                                                              Box<dyn Stream<Item=Result<String, Error>> + 'a>);
}

pub enum IconHint {
    /// No custom icon.
    Builtin(DepotType),
    /// The icon is a file in the installed artifact.
    File(PathBuf),
}

pub struct DepotFile {
    path: PathBuf,
    blake3: [u8; 32],
    byte_length: u64
}

impl DepotFile {
    pub fn new(path: PathBuf, blake3: [u8; 32], byte_length: u64) -> Self {
        Self { path, blake3, byte_length }
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

