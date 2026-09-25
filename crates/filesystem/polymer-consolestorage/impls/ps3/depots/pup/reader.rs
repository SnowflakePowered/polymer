use crate::installable::{InstallEvent, InstallEvents, Reporting};
use hmac::{Hmac, Mac};
use sha1::Sha1;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use zerocopy::byteorder::big_endian::U64;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

// PS3 PUP authentication key, as documented by PSDevWiki and RPCS3.
pub(super) const PUP_KEY: [u8; 64] = [
    0xF4, 0x91, 0xAD, 0x94, 0xC6, 0x81, 0x10, 0x96, 0x91, 0x5F, 0xD5, 0xD2, 0x44, 0x81, 0xAE, 0xDC,
    0xED, 0xED, 0xBE, 0x6B, 0xE5, 0x13, 0x72, 0x4D, 0xD8, 0xF7, 0xB6, 0x91, 0xE8, 0x8A, 0x38, 0xF4,
    0xB5, 0x16, 0x2B, 0xFB, 0xEC, 0xBE, 0x3A, 0x62, 0x18, 0x5D, 0xD7, 0xC9, 0x4D, 0xA2, 0x22, 0x5A,
    0xDA, 0x3F, 0xBF, 0xCE, 0x55, 0x5B, 0x9E, 0xA9, 0x64, 0x98, 0x29, 0xEB, 0x30, 0xCE, 0x83, 0x66,
];
#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Default)]
#[repr(C)]
struct Header {
    magic: [u8; 8],
    package_version: U64,
    image_version: U64,
    count: U64,
    header_size: U64,
    data_size: U64,
}
#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Default)]
#[repr(C)]
struct Entry {
    id: U64,
    offset: U64,
    size: U64,
    padding: [u8; 8],
}

pub(super) struct Pup {
    pub file: File,
    entries: Vec<Entry>,
    hashes: Vec<[u8; 20]>,
}
impl Pup {
    pub fn open(path: &Path) -> io::Result<Self> {
        let mut file = File::open(path)?;
        let mut header = Header::default();
        file.read_exact(header.as_mut_bytes())?;
        let count = header.count.get();
        let length = file.metadata()?.len();
        if header.magic != *b"SCEUF\0\0\0"
            || count == 0
            || count > 4096
            || header.header_size.get() < 48 + count * 64
            || header.header_size.get().checked_add(header.data_size.get()) != Some(length)
        {
            return Err(io::Error::other("invalid PUP header"));
        }
        let mut entries = Vec::new();
        for _ in 0..count {
            let mut entry = Entry::default();
            file.read_exact(entry.as_mut_bytes())?;
            if entry.offset.get() < header.header_size.get()
                || entry
                    .offset
                    .get()
                    .checked_add(entry.size.get())
                    .is_none_or(|end| end > length)
                || entries.iter().any(|old: &Entry| old.id == entry.id)
            {
                return Err(io::Error::other("invalid PUP entry"));
            }
            entries.push(entry);
        }
        let mut hashes = Vec::new();
        for _ in 0..count {
            let mut record = [0; 32];
            file.read_exact(&mut record)?;
            hashes.push(record[8..28].try_into().unwrap());
        }
        Ok(Self {
            file,
            entries,
            hashes,
        })
    }
    pub fn entry(&mut self, id: u64) -> io::Result<(u64, u64)> {
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.id.get() == id)
            .ok_or_else(|| io::Error::other(format!("missing PUP entry {id:#x}")))?;
        self.file.seek(SeekFrom::Start(entry.offset.get()))?;
        Ok((entry.offset.get(), entry.size.get()))
    }
    pub fn version(&mut self) -> io::Result<String> {
        let (_, size) = self.entry(0x100)?;
        if size > 128 {
            return Err(io::Error::other("invalid firmware version"));
        }
        let mut version = String::new();
        (&mut self.file).take(size).read_to_string(&mut version)?;
        let version = version.trim_matches(['\0', '\n', '\r', ' ']);
        if version.is_empty() || !version.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
            return Err(io::Error::other("invalid firmware version"));
        }
        Ok(version.to_owned())
    }
    /// Authenticate every entry, publishing the bytes read as a verifying phase.
    pub fn verify(&mut self, events: &InstallEvents) -> io::Result<()> {
        events(InstallEvent::Phase {
            name: "Verifying".to_owned(),
            total: self.entries.iter().map(|entry| entry.size.get()).sum(),
        });
        let mut buffer = [0; 64 * 1024];
        for (entry, expected) in self.entries.iter().zip(&self.hashes) {
            self.file.seek(SeekFrom::Start(entry.offset.get()))?;
            let mut reader = Reporting {
                inner: (&mut self.file).take(entry.size.get()),
                events: events.clone(),
            };
            let mut mac = Hmac::<Sha1>::new_from_slice(&PUP_KEY).unwrap();
            loop {
                let n = reader.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                mac.update(&buffer[..n]);
            }
            if reader.inner.limit() != 0 || mac.verify_slice(expected).is_err() {
                return Err(io::Error::other(format!(
                    "PUP entry {:#x} failed authentication",
                    entry.id.get()
                )));
            }
        }
        Ok(())
    }
}
