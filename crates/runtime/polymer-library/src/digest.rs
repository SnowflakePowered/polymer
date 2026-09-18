use sha2::digest::{Digest, FixedOutput, HashMarker, Output, OutputSizeUser, Update, consts::U4};
use std::io::{self, Read};

/// Supported digest algorithms. `sha2` means SHA-256 in serialized requests.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DigestAlgorithm {
    Sha2,
    Sha1,
    Md5,
    Crc32,
    Blake3,
}

impl DigestAlgorithm {
    /// Hash from the reader's current position through EOF as lowercase hexadecimal.
    /// Callers own source access and positioning; this performs blocking I/O.
    pub fn hash(self, reader: impl Read) -> io::Result<String> {
        match self {
            Self::Sha2 => Self::hash_reader::<sha2::Sha256>(reader),
            Self::Sha1 => Self::hash_reader::<sha1::Sha1>(reader),
            Self::Md5 => Self::hash_reader::<md5::Md5>(reader),
            Self::Crc32 => Self::hash_reader::<Crc32>(reader),
            Self::Blake3 => Self::hash_reader::<blake3::Hasher>(reader),
        }
    }

    fn hash_reader<D: Digest>(mut reader: impl Read) -> io::Result<String> {
        let mut digest = D::new();
        let mut buffer = vec![0; 1024 * 1024];
        loop {
            let count = match reader.read(&mut buffer) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => result?,
            };
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
        Ok(Self::hex(&digest.finalize()))
    }

    /// Format digest bytes produced by source-specific hashing APIs.
    pub fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write;
        let mut result = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            write!(result, "{byte:02x}").unwrap();
        }
        result
    }
}

/// Adapt CRC32 to the RustCrypto digest interface.
#[derive(Default)]
pub struct Crc32(crc32fast::Hasher);

impl OutputSizeUser for Crc32 {
    type OutputSize = U4;
}
impl HashMarker for Crc32 {}
impl Update for Crc32 {
    fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }
}
impl FixedOutput for Crc32 {
    fn finalize_into(self, output: &mut Output<Self>) {
        output.copy_from_slice(&self.0.finalize().to_be_bytes());
    }
}
