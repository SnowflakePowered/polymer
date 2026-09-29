use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::impls::ps2) enum ParseError {
    TooShort(usize),
    InvalidLayout,
    InvalidMagic,
    InvalidVersion,
    InvalidGeometry(&'static str),
    InvalidRootDirectory(usize),
    InvalidBackupBlocks([usize; 2]),
    InvalidCardType(u8),
    TooManyIndirectFatClusters(usize),
    InvalidIndirectFatCluster {
        index: usize,
        cluster: usize,
    },
    InvalidCardSize {
        actual: usize,
        data: usize,
        raw: usize,
    },
    InvalidCardOffset(usize),
    NonContiguousField {
        offset: usize,
        len: usize,
    },
    ClusterOutOfRange(usize),
    FatEntryOutOfRange(usize),
    DirentOutOfRange(usize),
    MissingIndirectFatCluster(usize),
    InvalidFatCluster {
        index: usize,
        cluster: usize,
    },
    InvalidFatNext {
        cluster: usize,
        next: usize,
    },
    UnallocatedCluster(usize),
    DirectoryTooLarge(usize),
    InvalidDirectory(&'static str),
    ChainEndedEarly {
        start: usize,
        required: usize,
    },
    ChainLoop(usize),
    InvalidFile(&'static str),
    FileTooLarge,
    NameTooLong(usize),
    ValueTooLarge(usize),
    Full {
        needed: usize,
        available: usize,
    },
    DuplicateSave(String),
    ReadOnly,
    Psu(psu::ParseError),
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort(size) => write!(
                formatter,
                "raw image is {size} bytes, shorter than its {HEADER_SIZE}-byte superblock header"
            ),
            Self::InvalidLayout => formatter.write_str("superblock has an invalid byte layout"),
            Self::InvalidMagic => formatter.write_str("superblock has invalid magic"),
            Self::InvalidVersion => formatter.write_str("superblock has an invalid version"),
            Self::InvalidGeometry(reason) => {
                write!(formatter, "superblock has invalid geometry: {reason}")
            }
            Self::InvalidRootDirectory(cluster) => write!(
                formatter,
                "superblock root directory starts at relative cluster {cluster}, not zero"
            ),
            Self::InvalidBackupBlocks(blocks) => {
                write!(formatter, "superblock has invalid backup blocks {blocks:?}")
            }
            Self::InvalidCardType(card_type) => {
                write!(
                    formatter,
                    "superblock has card type {card_type}, not PS2 type 2"
                )
            }
            Self::TooManyIndirectFatClusters(count) => write!(
                formatter,
                "superblock requires {count} indirect FAT clusters but has only {IFC_ENTRIES} slots"
            ),
            Self::InvalidIndirectFatCluster { index, cluster } => write!(
                formatter,
                "superblock indirect FAT slot {index} points outside system space at cluster {cluster}"
            ),
            Self::InvalidCardSize { actual, data, raw } => write!(
                formatter,
                "memory-card image is {actual} bytes instead of {data} data bytes or {raw} raw bytes"
            ),
            Self::InvalidCardOffset(offset) => {
                write!(
                    formatter,
                    "memory-card offset {offset} is outside the image"
                )
            }
            Self::NonContiguousField { offset, len } => write!(
                formatter,
                "the {len}-byte field at memory-card offset {offset} crosses a page boundary"
            ),
            Self::ClusterOutOfRange(cluster) => {
                write!(formatter, "cluster {cluster} is outside the card")
            }
            Self::FatEntryOutOfRange(index) => {
                write!(formatter, "FAT entry {index} is outside its cluster")
            }
            Self::DirentOutOfRange(index) => {
                write!(formatter, "directory entry {index} is outside its cluster")
            }
            Self::MissingIndirectFatCluster(index) => {
                write!(formatter, "indirect FAT slot {index} is missing")
            }
            Self::InvalidFatCluster { index, cluster } => write!(
                formatter,
                "indirect FAT entry {index} points outside system space at cluster {cluster}"
            ),
            Self::InvalidFatNext { cluster, next } => write!(
                formatter,
                "FAT entry for cluster {cluster} points outside allocation space at cluster {next}"
            ),
            Self::UnallocatedCluster(cluster) => {
                write!(
                    formatter,
                    "cluster {cluster} is in a chain but is not allocated"
                )
            }
            Self::DirectoryTooLarge(count) => {
                write!(formatter, "directory claims an invalid {count} entries")
            }
            Self::InvalidDirectory(reason) => write!(formatter, "invalid directory: {reason}"),
            Self::ChainEndedEarly { start, required } => write!(
                formatter,
                "cluster chain from {start} ends before its required {required} clusters"
            ),
            Self::ChainLoop(start) => {
                write!(formatter, "cluster chain from {start} contains a loop")
            }
            Self::InvalidFile(reason) => write!(formatter, "invalid file: {reason}"),
            Self::FileTooLarge => formatter.write_str("memory-card file is too large"),
            Self::NameTooLong(length) => {
                write!(formatter, "memory-card name is {length} bytes")
            }
            Self::ValueTooLarge(value) => {
                write!(formatter, "memory-card value {value} is too large")
            }
            Self::Full { needed, available } => write!(
                formatter,
                "memory card needs {needed} data clusters but has {available}"
            ),
            Self::DuplicateSave(name) => {
                write!(formatter, "memory card already contains save `{name}`")
            }
            Self::ReadOnly => formatter.write_str("memory-card view is read-only"),
            Self::Psu(error) => write!(formatter, "cannot encode PSU: {error}"),
        }
    }
}

impl From<psu::ParseError> for ParseError {
    fn from(error: psu::ParseError) -> Self {
        Self::Psu(error)
    }
}

impl From<ParseError> for crate::Error {
    fn from(error: ParseError) -> Self {
        Self::MalformedMetadata {
            file: "PlayStation 2 memory card".to_owned(),
            reason: error.to_string(),
        }
    }
}
