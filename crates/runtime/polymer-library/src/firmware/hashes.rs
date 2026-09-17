use super::bios::BiosFile;
use sha2::Digest;
use std::io::{self, Read, Seek};

impl BiosFile {
    pub(super) fn calculate(file: &mut std::fs::File, algorithm: &str) -> io::Result<String> {
        file.rewind()?;
        match algorithm {
            "md5" => Self::hash::<md5::Md5>(file),
            "sha256" => Self::hash::<sha2::Sha256>(file),
            "blake3" => {
                let mut hasher = blake3::Hasher::new();
                hasher.update_reader(file)?;
                Ok(blake3::Hasher::finalize(&hasher).to_hex().to_string())
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported firmware hash",
            )),
        }
    }

    fn hash<D: Digest>(file: &mut std::fs::File) -> io::Result<String> {
        use std::fmt::Write;
        let mut digest = D::new();
        let mut buffer = [0; 64 * 1024];
        loop {
            let count = match file.read(&mut buffer) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => result?,
            };
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
        let bytes = digest.finalize();
        let mut result = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            write!(result, "{byte:02x}").unwrap();
        }
        Ok(result)
    }
}
