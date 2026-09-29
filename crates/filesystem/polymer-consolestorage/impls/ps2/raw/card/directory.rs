use super::*;

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub(super) struct Dirent {
    mode: U16,
    unused: U16,
    length: U32,
    created: [u8; 8],
    cluster: U32,
    directory_entry: U32,
    modified: [u8; 8],
    attributes: U32,
    reserved: [u8; 28],
    name: [u8; 32],
    unused_tail: [u8; 416],
}

const _: () = assert!(size_of::<Dirent>() == DIRENT_SIZE);

impl Dirent {
    pub(super) fn mode(&self) -> u16 {
        self.mode.get()
    }

    pub(super) fn length(&self) -> usize {
        self.length.get() as usize
    }

    pub(super) fn cluster(&self) -> Option<usize> {
        (self.cluster.get() != u32::MAX).then_some(self.cluster.get() as usize)
    }

    pub(super) fn directory_entry(&self) -> usize {
        self.directory_entry.get() as usize
    }

    pub(super) fn created(&self) -> &[u8; 8] {
        &self.created
    }

    pub(super) fn modified(&self) -> &[u8; 8] {
        &self.modified
    }

    pub(super) fn attributes(&self) -> u32 {
        self.attributes.get()
    }

    pub(super) fn name(&self) -> Cow<'_, str> {
        let (name, _) = SHIFT_JIS.decode_without_bom_handling(self.name_bytes());
        name
    }

    pub(super) fn name_bytes(&self) -> &[u8] {
        let end = self
            .name
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(self.name.len());
        &self.name[..end]
    }

    pub(super) fn is_used(&self) -> bool {
        self.mode() & USED_MODE != 0
    }

    pub(super) fn is_file(&self) -> bool {
        self.mode() & FILE_MODE != 0
    }

    pub(super) fn is_directory(&self) -> bool {
        self.mode() & DIRECTORY_MODE != 0
    }
}

impl fmt::Debug for Dirent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Dirent")
            .field("mode", &format_args!("{:#06x}", self.mode()))
            .field("length", &self.length())
            .field("created", &self.created())
            .field("cluster", &self.cluster())
            .field("directory_entry", &self.directory_entry())
            .field("modified", &self.modified())
            .field("attributes", &self.attributes())
            .field("name", &self.name())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FatEntry {
    Free,
    End,
    Next(usize),
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Fat<'card, 'image> {
    pub(super) card: &'card MemoryCard<'image>,
}

impl Fat<'_, '_> {
    pub(super) fn entry(self, cluster: usize) -> Result<FatEntry, ParseError> {
        let superblock = self.card.superblock();
        if cluster >= superblock.alloc_end() {
            return Err(ParseError::ClusterOutOfRange(cluster));
        }
        let entries_per_cluster = superblock.entries_per_cluster();
        let indirect_index = cluster / entries_per_cluster;
        let indirect_slot = indirect_index / entries_per_cluster;
        let indirect_cluster = superblock
            .indirect_fat_cluster(indirect_slot)
            .ok_or(ParseError::MissingIndirectFatCluster(indirect_slot))?;
        let fat_cluster = self
            .card
            .raw_cluster_u32(indirect_cluster, indirect_index % entries_per_cluster)?
            as usize;
        if fat_cluster == 0 || fat_cluster >= superblock.alloc_offset() {
            return Err(ParseError::InvalidFatCluster {
                index: indirect_index,
                cluster: fat_cluster,
            });
        }
        let raw = self
            .card
            .raw_cluster_u32(fat_cluster, cluster % entries_per_cluster)?;
        if raw & ALLOCATED == 0 {
            return Ok(FatEntry::Free);
        }
        let next = (raw & !ALLOCATED) as usize;
        if next == CHAIN_END as usize {
            return Ok(FatEntry::End);
        }
        if next >= superblock.alloc_end() {
            return Err(ParseError::InvalidFatNext { cluster, next });
        }
        Ok(FatEntry::Next(next))
    }
}

#[derive(Debug)]
pub(super) struct Directory<'card, 'image> {
    pub(super) card: &'card MemoryCard<'image>,
    pub(super) clusters: Vec<usize>,
    pub(super) count: usize,
}

impl Directory<'_, '_> {
    pub(super) fn entries(&self) -> impl ExactSizeIterator<Item = &Dirent> + '_ {
        let entries_per_cluster = self.card.superblock().cluster_size() / DIRENT_SIZE;
        (0..self.count).map(move |index| {
            let cluster = self.clusters[index / entries_per_cluster];
            self.card
                .dirent(cluster, index % entries_per_cluster)
                .expect("directory clusters and entry bounds are validated at construction")
        })
    }
}
