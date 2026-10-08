use super::*;

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub(super) struct RawSuperblock {
    magic: [u8; 28],
    version: [u8; 12],
    page_len: U16,
    pages_per_cluster: U16,
    pages_per_block: U16,
    unused: U16,
    clusters_per_card: U32,
    alloc_offset: U32,
    alloc_end: U32,
    rootdir_cluster: U32,
    backup_block1: U32,
    backup_block2: U32,
    padding: U64,
    ifc_list: [U32; IFC_ENTRIES],
    bad_block_list: [U32; 32],
    card_type: u8,
    card_flags: u8,
    trailing: [u8; 2],
}

const _: () = assert!(size_of::<RawSuperblock>() == HEADER_SIZE);

/// A validated, borrowed view of a PS2 memory-card superblock.
#[derive(Clone, Copy)]
pub(super) struct Superblock<'a> {
    raw: &'a RawSuperblock,
}

impl<'a> Superblock<'a> {
    pub(super) fn new(card: &'a [u8]) -> Result<Self, ParseError> {
        let header = card
            .get(..HEADER_SIZE)
            .ok_or(ParseError::TooShort(card.len()))?;
        let raw = RawSuperblock::ref_from_bytes(header).map_err(|_| ParseError::InvalidLayout)?;
        let superblock = Self { raw };
        superblock.validate()?;
        Ok(superblock)
    }

    pub(super) fn version(self) -> &'a str {
        let bytes = self.version_bytes();
        std::str::from_utf8(bytes).expect("the version is validated when the superblock is parsed")
    }

    pub(super) fn page_len(self) -> usize {
        usize::from(self.raw.page_len.get())
    }

    pub(super) fn pages_per_cluster(self) -> usize {
        usize::from(self.raw.pages_per_cluster.get())
    }

    pub(super) fn pages_per_block(self) -> usize {
        usize::from(self.raw.pages_per_block.get())
    }

    pub(super) fn clusters_per_card(self) -> usize {
        self.raw.clusters_per_card.get() as usize
    }

    pub(super) fn alloc_offset(self) -> usize {
        self.raw.alloc_offset.get() as usize
    }

    pub(super) fn alloc_end(self) -> usize {
        self.raw.alloc_end.get() as usize
    }

    pub(super) fn rootdir_cluster(self) -> usize {
        self.raw.rootdir_cluster.get() as usize
    }

    pub(super) fn backup_blocks(self) -> [usize; 2] {
        [
            self.raw.backup_block1.get() as usize,
            self.raw.backup_block2.get() as usize,
        ]
    }

    pub(super) fn card_type(self) -> u8 {
        self.raw.card_type
    }

    pub(super) fn card_flags(self) -> u8 {
        self.raw.card_flags
    }

    pub(super) fn cluster_size(self) -> usize {
        self.page_len() * self.pages_per_cluster()
    }

    pub(super) fn erase_block_size(self) -> usize {
        self.page_len() * self.pages_per_block()
    }

    pub(super) fn entries_per_cluster(self) -> usize {
        self.cluster_size() / FAT_ENTRY_SIZE
    }

    pub(super) fn indirect_fat_cluster(self, index: usize) -> Option<usize> {
        self.raw
            .ifc_list
            .get(index)
            .map(|cluster| cluster.get() as usize)
    }

    fn version_bytes(self) -> &'a [u8] {
        let end = self
            .raw
            .version
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(self.raw.version.len());
        &self.raw.version[..end]
    }

    fn required_ifc_entries(self) -> usize {
        let entries_per_cluster = self.entries_per_cluster();
        self.alloc_end()
            .div_ceil(entries_per_cluster)
            .div_ceil(entries_per_cluster)
    }

    fn validate(self) -> Result<(), ParseError> {
        if self.raw.magic != *MAGIC {
            return Err(ParseError::InvalidMagic);
        }
        if self.version_bytes().is_empty() || std::str::from_utf8(self.version_bytes()).is_err() {
            return Err(ParseError::InvalidVersion);
        }
        if self.page_len() == 0 {
            return Err(ParseError::InvalidGeometry("page length is zero"));
        }
        if !self.page_len().is_multiple_of(DIRENT_SIZE) {
            return Err(ParseError::InvalidGeometry(
                "page length is not divisible by a directory entry",
            ));
        }
        if !self.page_len().is_multiple_of(ECC_CHUNK_SIZE) {
            return Err(ParseError::InvalidGeometry(
                "page length is not divisible by an ECC chunk",
            ));
        }
        if self.pages_per_cluster() == 0 {
            return Err(ParseError::InvalidGeometry("pages per cluster is zero"));
        }
        if self.pages_per_block() == 0 {
            return Err(ParseError::InvalidGeometry("pages per block is zero"));
        }
        let cluster_size = self
            .page_len()
            .checked_mul(self.pages_per_cluster())
            .ok_or(ParseError::InvalidGeometry("cluster size overflows"))?;
        if !cluster_size.is_multiple_of(FAT_ENTRY_SIZE) {
            return Err(ParseError::InvalidGeometry(
                "cluster size is not divisible by a FAT entry",
            ));
        }
        if !self
            .pages_per_block()
            .is_multiple_of(self.pages_per_cluster())
        {
            return Err(ParseError::InvalidGeometry(
                "an erase block does not contain whole clusters",
            ));
        }
        if self.clusters_per_card() == 0 {
            return Err(ParseError::InvalidGeometry("card has no clusters"));
        }
        if self.alloc_offset() == 0 || self.alloc_offset() > self.clusters_per_card() {
            return Err(ParseError::InvalidGeometry(
                "allocation offset is outside the card",
            ));
        }
        if self.alloc_end() == 0
            || self
                .alloc_offset()
                .checked_add(self.alloc_end())
                .is_none_or(|end| end > self.clusters_per_card())
        {
            return Err(ParseError::InvalidGeometry(
                "allocatable area is outside the card",
            ));
        }
        if self.rootdir_cluster() != 0 {
            return Err(ParseError::InvalidRootDirectory(self.rootdir_cluster()));
        }

        let pages_per_card = self
            .clusters_per_card()
            .checked_mul(self.pages_per_cluster())
            .ok_or(ParseError::InvalidGeometry("card page count overflows"))?;
        if !pages_per_card.is_multiple_of(self.pages_per_block()) {
            return Err(ParseError::InvalidGeometry(
                "card does not contain whole erase blocks",
            ));
        }
        let blocks_per_card = pages_per_card / self.pages_per_block();
        let [backup1, backup2] = self.backup_blocks();
        if backup1 >= blocks_per_card || backup2 >= blocks_per_card || backup1 == backup2 {
            return Err(ParseError::InvalidBackupBlocks([backup1, backup2]));
        }
        if self.card_type() != 2 {
            return Err(ParseError::InvalidCardType(self.card_type()));
        }

        let required_ifc_entries = self.required_ifc_entries();
        if required_ifc_entries > IFC_ENTRIES {
            return Err(ParseError::TooManyIndirectFatClusters(required_ifc_entries));
        }
        for index in 0..required_ifc_entries {
            let cluster = self
                .indirect_fat_cluster(index)
                .expect("the required IFC count is bounded by the field length");
            if cluster == 0 || cluster >= self.alloc_offset() {
                return Err(ParseError::InvalidIndirectFatCluster { index, cluster });
            }
        }
        Ok(())
    }
}

impl fmt::Debug for Superblock<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Superblock")
            .field("version", &self.version())
            .field("page_len", &self.page_len())
            .field("pages_per_cluster", &self.pages_per_cluster())
            .field("pages_per_block", &self.pages_per_block())
            .field("erase_block_size", &self.erase_block_size())
            .field("clusters_per_card", &self.clusters_per_card())
            .field("alloc_offset", &self.alloc_offset())
            .field("alloc_end", &self.alloc_end())
            .field("rootdir_cluster", &self.rootdir_cluster())
            .field("backup_blocks", &self.backup_blocks())
            .field("card_type", &self.card_type())
            .field("card_flags", &self.card_flags())
            .finish()
    }
}
