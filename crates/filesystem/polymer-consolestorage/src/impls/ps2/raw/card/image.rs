use super::*;

/// A borrowed view over a plain or spare/ECC-bearing PS2 card image.
#[derive(Debug)]
pub(in crate::impls::ps2) struct MemoryCard<'a> {
    pub(super) bytes: CardBytes<'a>,
    pub(super) page_stride: usize,
    pub(super) free_clusters: Vec<usize>,
}

impl<'a> MemoryCard<'a> {
    pub(in crate::impls::ps2) fn validate_superblock(
        bytes: &[u8],
        blocks: u32,
    ) -> Result<Superblock<'_>, ParseError> {
        if blocks > MAX_BLOCKS {
            return Err(ParseError::InvalidGeometry(
                "card exceeds the maximum supported capacity",
            ));
        }
        let geometry = Geometry::new(blocks)?;
        let superblock = Superblock::new(bytes)?;
        if superblock.clusters_per_card() != geometry.clusters_per_card as usize {
            return Err(ParseError::InvalidGeometry(
                "superblock capacity does not match the requested block count",
            ));
        }
        Ok(superblock)
    }

    pub(super) fn validate(bytes: &[u8], blocks: u32) -> Result<usize, ParseError> {
        let superblock = Self::validate_superblock(bytes, blocks)?;
        let data_size = superblock
            .clusters_per_card()
            .checked_mul(superblock.cluster_size())
            .ok_or(ParseError::InvalidGeometry("card size overflows"))?;
        let page_count = superblock
            .clusters_per_card()
            .checked_mul(superblock.pages_per_cluster())
            .ok_or(ParseError::InvalidGeometry("card page count overflows"))?;
        let spare_size = superblock.page_len() / ECC_CHUNK_SIZE * ECC_CHUNK_SPARE_SIZE;
        let raw_page_size = superblock
            .page_len()
            .checked_add(spare_size)
            .ok_or(ParseError::InvalidGeometry("raw page size overflows"))?;
        let raw_size = page_count
            .checked_mul(raw_page_size)
            .ok_or(ParseError::InvalidGeometry("raw card size overflows"))?;
        let page_stride = if bytes.len() == data_size {
            superblock.page_len()
        } else if bytes.len() == raw_size {
            raw_page_size
        } else {
            return Err(ParseError::InvalidCardSize {
                actual: bytes.len(),
                data: data_size,
                raw: raw_size,
            });
        };
        Ok(page_stride)
    }

    pub(super) fn superblock(&self) -> Superblock<'_> {
        Superblock::new(self.bytes.as_slice()).expect("a memory card is validated when constructed")
    }

    pub(super) fn scan_free_clusters(&self) -> Result<Vec<usize>, ParseError> {
        (0..self.superblock().alloc_end())
            .filter_map(|cluster| match self.fat().entry(cluster) {
                Ok(FatEntry::Free) => Some(Ok(cluster)),
                Ok(FatEntry::End | FatEntry::Next(_)) => None,
                Err(error) => Some(Err(error)),
            })
            .collect()
    }

    pub(super) fn fat(&self) -> Fat<'_, 'a> {
        Fat { card: self }
    }

    pub(super) fn root_directory(&self) -> Result<Directory<'_, 'a>, ParseError> {
        let start = self.superblock().rootdir_cluster();
        let dot = self.dirent(start, 0)?;
        if !dot.is_used() || !dot.is_directory() || dot.name_bytes() != b"." {
            return Err(ParseError::InvalidDirectory(
                "first entry is not the used `.` directory entry",
            ));
        }
        self.directory_at(start, dot.length())
    }

    pub(super) fn directory(&self, dirent: &Dirent) -> Result<Directory<'_, 'a>, ParseError> {
        if !dirent.is_used() || !dirent.is_directory() {
            return Err(ParseError::InvalidDirectory(
                "entry does not name a used directory",
            ));
        }
        let start = dirent
            .cluster()
            .ok_or(ParseError::InvalidDirectory("directory has no cluster"))?;
        self.directory_at(start, dirent.length())
    }

    fn directory_at(&self, start: usize, count: usize) -> Result<Directory<'_, 'a>, ParseError> {
        if count < 2 {
            return Err(ParseError::InvalidDirectory(
                "directory has fewer than `.` and `..` entries",
            ));
        }
        let byte_len = count
            .checked_mul(DIRENT_SIZE)
            .ok_or(ParseError::DirectoryTooLarge(count))?;
        let superblock = self.superblock();
        let cluster_count = byte_len.div_ceil(superblock.cluster_size());
        if cluster_count > superblock.alloc_end() {
            return Err(ParseError::DirectoryTooLarge(count));
        }

        let mut clusters = Vec::with_capacity(cluster_count);
        let mut seen = vec![false; superblock.alloc_end()];
        let mut cluster = start;
        for index in 0..cluster_count {
            let slot = seen
                .get_mut(cluster)
                .ok_or(ParseError::ClusterOutOfRange(cluster))?;
            if std::mem::replace(slot, true) {
                return Err(ParseError::ChainLoop(start));
            }
            clusters.push(cluster);
            let fat_entry = self.fat().entry(cluster)?;
            if index + 1 < cluster_count {
                cluster = match fat_entry {
                    FatEntry::Next(next) => next,
                    FatEntry::End => {
                        return Err(ParseError::ChainEndedEarly {
                            start,
                            required: cluster_count,
                        });
                    }
                    FatEntry::Free => return Err(ParseError::UnallocatedCluster(cluster)),
                };
            } else if fat_entry == FatEntry::Free {
                return Err(ParseError::UnallocatedCluster(cluster));
            }
        }

        let directory = Directory {
            card: self,
            clusters,
            count,
        };
        let mut entries = directory.entries();
        let dot = entries
            .next()
            .expect("a directory has at least two entries");
        let dotdot = entries
            .next()
            .expect("a directory has at least two entries");
        if !dot.is_used() || !dot.is_directory() || dot.name_bytes() != b"." {
            return Err(ParseError::InvalidDirectory(
                "first entry is not the used `.` directory entry",
            ));
        }
        if !dotdot.is_used() || !dotdot.is_directory() || dotdot.name_bytes() != b".." {
            return Err(ParseError::InvalidDirectory(
                "second entry is not the used `..` directory entry",
            ));
        }
        drop(entries);
        Ok(directory)
    }

    pub(super) fn dirent(&self, cluster: usize, index: usize) -> Result<&Dirent, ParseError> {
        let superblock = self.superblock();
        if cluster >= superblock.alloc_end() {
            return Err(ParseError::ClusterOutOfRange(cluster));
        }
        let entries_per_cluster = superblock.cluster_size() / DIRENT_SIZE;
        if index >= entries_per_cluster {
            return Err(ParseError::DirentOutOfRange(index));
        }
        let absolute_cluster = superblock
            .alloc_offset()
            .checked_add(cluster)
            .ok_or(ParseError::InvalidCardOffset(cluster))?;
        let offset = absolute_cluster
            .checked_mul(superblock.cluster_size())
            .and_then(|offset| offset.checked_add(index * DIRENT_SIZE))
            .ok_or(ParseError::InvalidCardOffset(cluster))?;
        let bytes = self.data_slice(offset, DIRENT_SIZE)?;
        Dirent::ref_from_bytes(bytes).map_err(|_| ParseError::InvalidLayout)
    }

    pub(super) fn raw_cluster_u32(&self, cluster: usize, index: usize) -> Result<u32, ParseError> {
        let superblock = self.superblock();
        if cluster >= superblock.clusters_per_card() {
            return Err(ParseError::ClusterOutOfRange(cluster));
        }
        if index >= superblock.entries_per_cluster() {
            return Err(ParseError::FatEntryOutOfRange(index));
        }
        let offset = cluster
            .checked_mul(superblock.cluster_size())
            .and_then(|offset| offset.checked_add(index * FAT_ENTRY_SIZE))
            .ok_or(ParseError::InvalidCardOffset(cluster))?;
        let bytes = self.data_slice(offset, FAT_ENTRY_SIZE)?;
        let value = U32::ref_from_bytes(bytes).map_err(|_| ParseError::InvalidLayout)?;
        Ok(value.get())
    }

    pub(super) fn file_chunks(&self, dirent: &Dirent) -> Result<Vec<&[u8]>, ParseError> {
        let superblock = self.superblock();
        let length = dirent.length();
        if length == 0 {
            return Ok(Vec::new());
        }
        let start = dirent
            .cluster()
            .ok_or(ParseError::InvalidFile("non-empty file has no cluster"))?;
        let required_clusters = length.div_ceil(superblock.cluster_size());
        if required_clusters > superblock.alloc_end() {
            return Err(ParseError::InvalidFile(
                "file length exceeds allocation space",
            ));
        }

        let mut chunks = Vec::with_capacity(length.div_ceil(superblock.page_len()));
        let mut seen = vec![false; superblock.alloc_end()];
        let mut cluster = start;
        let mut remaining = length;
        for index in 0..required_clusters {
            let slot = seen
                .get_mut(cluster)
                .ok_or(ParseError::ClusterOutOfRange(cluster))?;
            if std::mem::replace(slot, true) {
                return Err(ParseError::ChainLoop(start));
            }

            let absolute_cluster = superblock
                .alloc_offset()
                .checked_add(cluster)
                .ok_or(ParseError::InvalidCardOffset(cluster))?;
            let cluster_offset = absolute_cluster
                .checked_mul(superblock.cluster_size())
                .ok_or(ParseError::InvalidCardOffset(cluster))?;
            for page in 0..superblock.pages_per_cluster() {
                if remaining == 0 {
                    break;
                }
                let take = remaining.min(superblock.page_len());
                chunks.push(self.data_slice(cluster_offset + page * superblock.page_len(), take)?);
                remaining -= take;
            }

            let fat_entry = self.fat().entry(cluster)?;
            if index + 1 < required_clusters {
                cluster = match fat_entry {
                    FatEntry::Next(next) => next,
                    FatEntry::End => {
                        return Err(ParseError::ChainEndedEarly {
                            start,
                            required: required_clusters,
                        });
                    }
                    FatEntry::Free => return Err(ParseError::UnallocatedCluster(cluster)),
                };
            } else if fat_entry == FatEntry::Free {
                return Err(ParseError::UnallocatedCluster(cluster));
            }
        }
        debug_assert_eq!(remaining, 0);
        Ok(chunks)
    }

    fn data_slice(&self, offset: usize, len: usize) -> Result<&[u8], ParseError> {
        let page_len = self.superblock().page_len();
        let page = offset / page_len;
        let within_page = offset % page_len;
        if len > page_len - within_page {
            return Err(ParseError::NonContiguousField { offset, len });
        }
        let physical = page
            .checked_mul(self.page_stride)
            .and_then(|offset| offset.checked_add(within_page))
            .ok_or(ParseError::InvalidCardOffset(offset))?;
        self.bytes
            .as_slice()
            .get(physical..physical + len)
            .ok_or(ParseError::InvalidCardOffset(offset))
    }
}
