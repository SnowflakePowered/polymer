use super::*;

#[derive(Clone, Copy, Debug)]
pub(super) struct Geometry {
    blocks: u32,
    pub(super) clusters_per_card: u32,
    pub(super) alloc_offset: u32,
    pub(super) alloc_end: u32,
    pub(super) ifc_cluster: u32,
    pub(super) fat_clusters: u32,
}

impl Geometry {
    pub(super) fn new(capacity_blocks: u32) -> Result<Self, ParseError> {
        let blocks = capacity_blocks;
        let clusters_per_block = CLUSTERS_PER_BLOCK as u32;
        let entries_per_cluster = ENTRIES_PER_CLUSTER as u32;
        if blocks <= RESERVED_BACKUP_BLOCKS as u32 {
            return Err(ParseError::InvalidGeometry(
                "card has no room outside its backup blocks",
            ));
        }
        let clusters_per_card = blocks
            .checked_mul(clusters_per_block)
            .ok_or(ParseError::InvalidGeometry("card cluster count overflows"))?;
        let reserved_tail = RESERVED_BACKUP_BLOCKS as u32 * clusters_per_block;
        let ifc_cluster = RESERVED_HEAD_CLUSTERS as u32;
        let mut fat_clusters = 1;
        loop {
            let alloc_offset = ifc_cluster + 1 + fat_clusters;
            let alloc_end = clusters_per_card
                .checked_sub(alloc_offset + reserved_tail)
                .ok_or(ParseError::InvalidGeometry(
                    "card has no allocatable clusters",
                ))?;
            let needed = alloc_end.div_ceil(entries_per_cluster);
            if needed <= fat_clusters {
                if fat_clusters > entries_per_cluster {
                    return Err(ParseError::InvalidGeometry(
                        "card requires more than one indirect FAT cluster",
                    ));
                }
                return Ok(Self {
                    blocks,
                    clusters_per_card,
                    alloc_offset,
                    alloc_end,
                    ifc_cluster,
                    fat_clusters,
                });
            }
            fat_clusters = needed;
        }
    }

    pub(super) fn data_size(self) -> usize {
        self.clusters_per_card as usize * CLUSTER_SIZE
    }

    pub(super) fn header(self) -> Result<[u8; HEADER_SIZE], ParseError> {
        let mut bytes = [0; HEADER_SIZE];
        bytes[..MAGIC.len()].copy_from_slice(MAGIC);
        bytes[0x1c..0x23].copy_from_slice(b"1.2.0.0");
        let mut cursor = Cursor::new(bytes.as_mut_slice());
        cursor.set_position(0x28);
        for value in [
            (PAGE_SIZE as u16).to_le_bytes(),
            (PAGES_PER_CLUSTER as u16).to_le_bytes(),
            (PAGES_PER_BLOCK as u16).to_le_bytes(),
            0xff00_u16.to_le_bytes(),
        ] {
            cursor.write_all(&value).expect("superblock fields fit");
        }
        for value in [
            self.clusters_per_card,
            self.alloc_offset,
            self.alloc_end,
            0,
            self.blocks - 1,
            self.blocks - 2,
        ] {
            cursor
                .write_all(&value.to_le_bytes())
                .expect("superblock fields fit");
        }
        cursor.set_position(0x50);
        cursor
            .write_all(&self.ifc_cluster.to_le_bytes())
            .expect("superblock fields fit");
        bytes[0xd0..0x150].fill(0xff);
        bytes[0x150] = 2;
        bytes[0x151] = 0x2b;
        Ok(bytes)
    }
}

pub(super) struct Allocator {
    pub(super) next: usize,
    pub(super) limit: usize,
}

impl Allocator {
    pub(super) fn take(&mut self, count: usize) -> Result<usize, ParseError> {
        let end = self.next.checked_add(count).ok_or(ParseError::Full {
            needed: usize::MAX,
            available: self.limit,
        })?;
        if end > self.limit {
            return Err(ParseError::Full {
                needed: end,
                available: self.limit,
            });
        }
        let start = self.next;
        self.next = end;
        Ok(start)
    }
}
