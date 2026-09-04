use std::borrow::Cow;
use std::fmt;

use encoding_rs::SHIFT_JIS;
use zerocopy::byteorder::little_endian::U32;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

pub(super) const EXTENSION: &str = "psu";

const ENTRY_SIZE: usize = 512;
const CONTENT_ALIGNMENT: usize = 1024;
const DIRECTORY_MODE: u32 = 0x8427;
const FILE_MODE: u32 = 0x8497;
const DIRECTORY_BIT: u32 = 0x20;
const FILE_BIT: u32 = 0x10;

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct Dirent {
    mode: U32,
    length: U32,
    created: [u8; 8],
    cluster: U32,
    directory_entry: U32,
    modified: [u8; 8],
    attributes: U32,
    reserved: [u8; 28],
    name: [u8; 32],
    unused: [u8; 416],
}

const _: () = assert!(size_of::<Dirent>() == ENTRY_SIZE);

impl Dirent {
    fn parse(bytes: &[u8]) -> Result<&Self, ParseError> {
        Self::ref_from_bytes(bytes).map_err(|_| ParseError::InvalidLayout)
    }

    fn name(&self) -> Cow<'_, str> {
        let (name, _) = SHIFT_JIS.decode_without_bom_handling(self.name_bytes());
        name
    }

    fn name_bytes(&self) -> &[u8] {
        let end = self
            .name
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(self.name.len());
        &self.name[..end]
    }

    fn is_directory(&self) -> bool {
        self.mode.get() & DIRECTORY_BIT != 0
    }

    fn is_file(&self) -> bool {
        self.mode.get() & FILE_BIT != 0
    }
}

/// Incrementally encodes borrowed card dirents and file-chain chunks into one owned PSU.
pub(super) struct Encoder {
    bytes: Vec<u8>,
    remaining_files: usize,
}

impl Encoder {
    pub(super) fn new(raw_directory: &[u8], file_count: usize) -> Result<Self, ParseError> {
        let count = file_count
            .checked_add(2)
            .and_then(|count| u32::try_from(count).ok())
            .ok_or(ParseError::TooManyFiles)?;
        if raw_directory.len() < ENTRY_SIZE {
            return Err(ParseError::InvalidLayout);
        }
        let mut directory = [0; ENTRY_SIZE];
        directory.copy_from_slice(&raw_directory[..ENTRY_SIZE]);
        let dirent =
            Dirent::mut_from_bytes(&mut directory).map_err(|_| ParseError::InvalidLayout)?;
        if !dirent.is_directory() {
            return Err(ParseError::InvalidDirectory);
        }
        dirent.length.set(count);

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&directory);
        bytes.extend_from_slice(&dot_dirent(&directory, b"."));
        bytes.extend_from_slice(&dot_dirent(&directory, b".."));
        Ok(Self {
            bytes,
            remaining_files: file_count,
        })
    }

    pub(super) fn file<'a>(
        &mut self,
        raw_dirent: &[u8],
        length: usize,
        chunks: impl IntoIterator<Item = &'a [u8]>,
    ) -> Result<(), ParseError> {
        if self.remaining_files == 0 {
            return Err(ParseError::TooManyFiles);
        }
        if raw_dirent.len() < ENTRY_SIZE {
            return Err(ParseError::InvalidLayout);
        }
        let length32 = u32::try_from(length).map_err(|_| ParseError::FileTooLarge)?;
        let mut encoded = [0; ENTRY_SIZE];
        encoded.copy_from_slice(&raw_dirent[..ENTRY_SIZE]);
        let dirent = Dirent::mut_from_bytes(&mut encoded).map_err(|_| ParseError::InvalidLayout)?;
        if !dirent.is_file() {
            return Err(ParseError::Subdirectory);
        }
        dirent.length.set(length32);
        self.bytes.extend_from_slice(&encoded);

        let start = self.bytes.len();
        for chunk in chunks {
            self.bytes.extend_from_slice(chunk);
        }
        let actual = self.bytes.len() - start;
        if actual != length {
            return Err(ParseError::FileLengthMismatch {
                expected: length,
                actual,
            });
        }
        self.bytes.resize(
            self.bytes
                .len()
                .checked_add(padding(length))
                .ok_or(ParseError::FileTooLarge)?,
            0,
        );
        self.remaining_files -= 1;
        Ok(())
    }

    pub(super) fn finish(self) -> Result<Save<'static>, ParseError> {
        if self.remaining_files != 0 {
            return Err(ParseError::InvalidEntryCount(self.remaining_files + 2));
        }
        Save::new(self.bytes)
    }
}

/// A borrowed or owned EMS Adapter PSU save.
#[derive(Clone, Debug)]
pub(crate) struct Save<'a> {
    bytes: Cow<'a, [u8]>,
}

#[derive(Clone, Copy)]
pub(crate) struct File<'a> {
    dirent: &'a Dirent,
    data: &'a [u8],
}

impl<'a> Save<'a> {
    pub(super) fn new(bytes: impl Into<Cow<'a, [u8]>>) -> Result<Self, ParseError> {
        let save = Self {
            bytes: bytes.into(),
        };
        save.parse()?;
        Ok(save)
    }

    pub(super) fn from_files<'b>(
        name: &str,
        raw_dirent: &[u8],
        files: impl IntoIterator<Item = (&'b str, &'b [u8], &'b [u8])>,
    ) -> Result<Save<'static>, ParseError> {
        let files = files.into_iter().collect::<Vec<_>>();
        let count = files
            .len()
            .checked_add(2)
            .and_then(|count| u32::try_from(count).ok())
            .ok_or(ParseError::TooManyFiles)?;
        let mut bytes = Vec::new();
        let directory = encode_dirent(raw_dirent, name, DIRECTORY_MODE, count)?;
        validate_encoded_name(&directory, name)?;
        bytes.extend_from_slice(&directory);
        bytes.extend_from_slice(&dot_dirent(&directory, b"."));
        bytes.extend_from_slice(&dot_dirent(&directory, b".."));

        for (name, raw_dirent, data) in files {
            let length = u32::try_from(data.len()).map_err(|_| ParseError::FileTooLarge)?;
            let dirent = encode_dirent(raw_dirent, name, FILE_MODE, length)?;
            validate_encoded_name(&dirent, name)?;
            bytes.extend_from_slice(&dirent);
            bytes.extend_from_slice(data);
            bytes.resize(
                bytes
                    .len()
                    .checked_add(padding(data.len()))
                    .ok_or(ParseError::FileTooLarge)?,
                0,
            );
        }

        Save::new(bytes)
    }

    pub(super) fn name(&self) -> Cow<'_, str> {
        self.directory().name()
    }

    pub(super) fn name_bytes(&self) -> &[u8] {
        self.directory().name_bytes()
    }

    pub(super) fn raw_dirent(&self) -> &[u8] {
        self.directory().as_bytes()
    }

    pub(crate) fn files(&self) -> impl ExactSizeIterator<Item = File<'_>> {
        self.parse()
            .expect("a Save is validated when it is constructed")
            .into_iter()
    }

    pub(super) fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn directory(&self) -> &Dirent {
        Dirent::parse(&self.bytes[..ENTRY_SIZE])
            .expect("a Save is validated when it is constructed")
    }

    fn parse(&self) -> Result<Vec<File<'_>>, ParseError> {
        let bytes = self.bytes.as_ref();
        if bytes.len() < ENTRY_SIZE * 3 {
            return Err(ParseError::TooShort(bytes.len()));
        }
        let directory = Dirent::parse(&bytes[..ENTRY_SIZE])?;
        let dot = Dirent::parse(&bytes[ENTRY_SIZE..ENTRY_SIZE * 2])?;
        let dotdot = Dirent::parse(&bytes[ENTRY_SIZE * 2..ENTRY_SIZE * 3])?;
        if !directory.is_directory() {
            return Err(ParseError::InvalidDirectory);
        }
        if !dot.is_directory() || dot.name_bytes() != b"." {
            return Err(ParseError::InvalidDot);
        }
        if !dotdot.is_directory() || dotdot.name_bytes() != b".." {
            return Err(ParseError::InvalidDotDot);
        }
        let count =
            usize::try_from(directory.length.get()).map_err(|_| ParseError::TooManyFiles)?;
        let file_count = count
            .checked_sub(2)
            .ok_or(ParseError::InvalidEntryCount(count))?;
        let mut cursor = ENTRY_SIZE * 3;
        let mut files = Vec::with_capacity(file_count);
        for _ in 0..file_count {
            let end = cursor
                .checked_add(ENTRY_SIZE)
                .ok_or(ParseError::FileTooLarge)?;
            let dirent = Dirent::parse(bytes.get(cursor..end).ok_or(ParseError::TruncatedEntry)?)?;
            if !dirent.is_file() {
                return Err(ParseError::Subdirectory);
            }
            cursor = end;
            let length =
                usize::try_from(dirent.length.get()).map_err(|_| ParseError::FileTooLarge)?;
            let end = cursor.checked_add(length).ok_or(ParseError::FileTooLarge)?;
            let data = bytes.get(cursor..end).ok_or(ParseError::TruncatedFile)?;
            files.push(File { dirent, data });
            cursor = end
                .checked_add(padding(length))
                .ok_or(ParseError::FileTooLarge)?;
            if cursor > bytes.len() {
                return Err(ParseError::TruncatedPadding);
            }
        }
        if cursor != bytes.len() {
            return Err(ParseError::TrailingBytes(bytes.len() - cursor));
        }
        Ok(files)
    }
}

impl<'a> File<'a> {
    pub(super) fn name(&self) -> Cow<'a, str> {
        self.dirent.name()
    }

    pub(crate) fn name_bytes(&self) -> &'a [u8] {
        self.dirent.name_bytes()
    }

    pub(super) fn raw_dirent(&self) -> &'a [u8] {
        self.dirent.as_bytes()
    }

    pub(crate) fn data(&self) -> &'a [u8] {
        self.data
    }
}

fn padding(length: usize) -> usize {
    (CONTENT_ALIGNMENT - length % CONTENT_ALIGNMENT) % CONTENT_ALIGNMENT
}

fn validate_encoded_name(bytes: &[u8; ENTRY_SIZE], expected: &str) -> Result<(), ParseError> {
    let dirent = Dirent::ref_from_bytes(bytes).map_err(|_| ParseError::InvalidLayout)?;
    let actual = dirent.name();
    if actual.as_ref() != expected {
        return Err(ParseError::NameMismatch {
            expected: expected.to_owned(),
            actual: actual.into_owned(),
        });
    }
    Ok(())
}

fn encode_dirent(
    raw: &[u8],
    name: &str,
    default_mode: u32,
    length: u32,
) -> Result<[u8; ENTRY_SIZE], ParseError> {
    let mut bytes = [0; ENTRY_SIZE];
    if raw.len() >= ENTRY_SIZE {
        bytes.copy_from_slice(&raw[..ENTRY_SIZE]);
    }
    let dirent = Dirent::mut_from_bytes(&mut bytes).map_err(|_| ParseError::InvalidLayout)?;
    if dirent.mode.get() == 0 {
        dirent.mode.set(default_mode);
    }
    dirent.length.set(length);
    if raw.len() < ENTRY_SIZE {
        let (name, _, had_errors) = SHIFT_JIS.encode(name);
        if had_errors {
            return Err(ParseError::UnencodableName);
        }
        if name.len() > dirent.name.len() {
            return Err(ParseError::NameTooLong(name.len()));
        }
        dirent.name[..name.len()].copy_from_slice(&name);
    }
    Ok(bytes)
}

fn dot_dirent(directory: &[u8; ENTRY_SIZE], name: &[u8]) -> [u8; ENTRY_SIZE] {
    let directory = Dirent::ref_from_bytes(directory).expect("a dirent has a verified layout");
    let mut bytes = [0; ENTRY_SIZE];
    let dot = Dirent::mut_from_bytes(&mut bytes).expect("a dirent has a verified layout");
    dot.mode.set(DIRECTORY_MODE);
    dot.created = directory.created;
    dot.modified = directory.modified;
    dot.name[..name.len()].copy_from_slice(name);
    bytes
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ParseError {
    TooShort(usize),
    InvalidLayout,
    InvalidDirectory,
    InvalidDot,
    InvalidDotDot,
    InvalidEntryCount(usize),
    TruncatedEntry,
    TruncatedFile,
    TruncatedPadding,
    TrailingBytes(usize),
    Subdirectory,
    TooManyFiles,
    FileTooLarge,
    FileLengthMismatch { expected: usize, actual: usize },
    NameTooLong(usize),
    UnencodableName,
    NameMismatch { expected: String, actual: String },
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort(length) => write!(formatter, "PSU is only {length} bytes"),
            Self::InvalidLayout => formatter.write_str("PSU has an invalid dirent layout"),
            Self::InvalidDirectory => formatter.write_str("PSU does not begin with a directory"),
            Self::InvalidDot => formatter.write_str("PSU has an invalid `.` entry"),
            Self::InvalidDotDot => formatter.write_str("PSU has an invalid `..` entry"),
            Self::InvalidEntryCount(count) => {
                write!(formatter, "PSU directory claims only {count} entries")
            }
            Self::TruncatedEntry => formatter.write_str("PSU ends inside a file dirent"),
            Self::TruncatedFile => formatter.write_str("PSU ends inside file data"),
            Self::TruncatedPadding => formatter.write_str("PSU ends inside file padding"),
            Self::TrailingBytes(length) => write!(formatter, "PSU has {length} trailing bytes"),
            Self::Subdirectory => formatter.write_str("PSU contains a nested directory"),
            Self::TooManyFiles => formatter.write_str("PSU contains too many files"),
            Self::FileTooLarge => formatter.write_str("PSU file data is too large"),
            Self::FileLengthMismatch { expected, actual } => write!(
                formatter,
                "PSU file contains {actual} bytes instead of its declared {expected}"
            ),
            Self::NameTooLong(length) => write!(formatter, "PSU name is {length} bytes"),
            Self::UnencodableName => {
                formatter.write_str("PSU name is not representable in Shift-JIS")
            }
            Self::NameMismatch { expected, actual } => {
                write!(
                    formatter,
                    "PSU dirent name `{actual}` does not match `{expected}`"
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borrowed_and_owned_save_round_trip_raw_dirents_and_aligned_files() {
        let mut directory = vec![0; ENTRY_SIZE];
        directory[..4].copy_from_slice(&DIRECTORY_MODE.to_le_bytes());
        directory[8] = 0x19;
        directory[0x40..0x4c].copy_from_slice(b"BASLUS-12345");
        let mut first = vec![0; ENTRY_SIZE];
        first[..4].copy_from_slice(&FILE_MODE.to_le_bytes());
        first[0x20..0x24].copy_from_slice(&0x1234_5678_u32.to_le_bytes());
        first[0x40..0x48].copy_from_slice(b"save.dat");
        let empty = Vec::new();
        let owned = Save::from_files(
            "BASLUS-12345",
            &directory,
            [
                ("save.dat", first.as_slice(), vec![0x5a; 1024]),
                ("empty", empty.as_slice(), Vec::new()),
            ]
            .iter()
            .map(|(name, dirent, data)| (*name, *dirent, data.as_slice())),
        )
        .unwrap();
        assert_eq!(owned.as_bytes().len(), ENTRY_SIZE * 5 + 1024);

        let borrowed = Save::new(owned.as_bytes()).unwrap();
        assert_eq!(borrowed.name(), "BASLUS-12345");
        assert_eq!(borrowed.raw_dirent()[8], 0x19);
        let files = borrowed.files().collect::<Vec<_>>();
        assert_eq!(files[0].name(), "save.dat");
        assert_eq!(
            files[0].raw_dirent()[0x20..0x24],
            0x1234_5678_u32.to_le_bytes()
        );
        assert_eq!(files[0].data(), vec![0x5a; 1024]);
        assert_eq!(files[1].name(), "empty");
        assert!(files[1].data().is_empty());
    }

    #[test]
    fn exposes_native_name_bytes_without_requiring_valid_shift_jis() {
        let save = Save::from_files("BASLUS-12345", &[], []).unwrap();
        let mut bytes = save.as_bytes().to_vec();
        bytes[0x40..0x60].fill(0);
        bytes[0x40..0x43].copy_from_slice(&[0x82, 0xff, b'A']);

        let save = Save::new(bytes).unwrap();
        assert_eq!(save.name_bytes(), &[0x82, 0xff, b'A']);
    }
}
