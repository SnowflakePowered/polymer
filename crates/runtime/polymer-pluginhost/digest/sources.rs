use super::DigestAlgorithm;
use gumshoe::{GeneralIdentifier, IdentifyPackage, binary::NormalizationOptions};
use polymer_library::digest::Crc32;
use polymer_vfs::{FileNode, OpenFile, OpenOptions};
use std::{
    io::{self, Read},
    path::Path,
};

/// Hash a source on demand. Callers serving async APIs run this blocking work
/// on a worker thread. Implementations stream bytes without buffering a whole file.
pub trait Digestible {
    fn digest(&self, algorithm: DigestAlgorithm) -> io::Result<String>;
}

impl Digestible for Path {
    fn digest(&self, algorithm: DigestAlgorithm) -> io::Result<String> {
        algorithm.hash(std::fs::File::open(self)?)
    }
}

impl Digestible for dyn FileNode {
    fn digest(&self, algorithm: DigestAlgorithm) -> io::Result<String> {
        let file = self
            .open(OpenOptions {
                read: true,
                write: false,
                truncate: false,
            })
            .map_err(io::Error::other)?;
        algorithm.hash(VfsReader { file, offset: 0 })
    }
}

struct VfsReader {
    file: Box<dyn OpenFile>,
    offset: u64,
}

impl Read for VfsReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = self
            .file
            .read(self.offset, buffer)
            .map_err(io::Error::other)?;
        self.offset += count as u64;
        Ok(count)
    }
}

/// The first normalized executable in a package, as used for library identity.
pub struct PackageBinary<'a>(pub &'a Path);

impl Digestible for PackageBinary<'_> {
    fn digest(&self, algorithm: DigestAlgorithm) -> io::Result<String> {
        let package = GeneralIdentifier
            .identify_path(self.0)
            .map_err(io::Error::other)?;
        let binary = package
            .binaries(NormalizationOptions::decrypt_common())
            .next()
            .ok_or_else(|| io::Error::other("package has no executable binary"))?
            .map_err(io::Error::other)?;
        Digestible::digest(&binary, algorithm)
    }
}

impl<R: gumshoe::ExactSizedStream> Digestible for gumshoe::binary::Binary<'_, R> {
    fn digest(&self, algorithm: DigestAlgorithm) -> io::Result<String> {
        let mut scratch = vec![0; 1024 * 1024];
        let bytes = match algorithm {
            DigestAlgorithm::Sha2 => self.digest::<sha2::Sha256>(Some(&mut scratch))?.to_vec(),
            DigestAlgorithm::Sha1 => self.digest::<sha1::Sha1>(Some(&mut scratch))?.to_vec(),
            DigestAlgorithm::Md5 => self.digest::<md5::Md5>(Some(&mut scratch))?.to_vec(),
            DigestAlgorithm::Crc32 => self.digest::<Crc32>(Some(&mut scratch))?.to_vec(),
            DigestAlgorithm::Blake3 => self.digest::<blake3::Hasher>(Some(&mut scratch))?.to_vec(),
        };
        Ok(DigestAlgorithm::hex(&bytes))
    }
}
