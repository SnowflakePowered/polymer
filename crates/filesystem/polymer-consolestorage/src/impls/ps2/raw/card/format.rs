use super::*;

impl MemoryCard<'static> {
    pub(in crate::impls::ps2) fn fits(
        capacity_blocks: u32,
        saves: &[Save<'_>],
    ) -> Result<bool, ParseError> {
        let geometry = Geometry::new(capacity_blocks)?;
        Ok(Self::required_clusters(saves)? <= geometry.alloc_end as usize)
    }

    pub(in crate::impls::ps2) fn format(
        capacity_blocks: u32,
        saves: &[Save<'_>],
        with_spare: bool,
    ) -> Result<Vec<u8>, ParseError> {
        let geometry = Geometry::new(capacity_blocks)?;
        if !Self::fits(capacity_blocks, saves)? {
            return Err(ParseError::Full {
                needed: Self::required_clusters(saves)?,
                available: geometry.alloc_end as usize,
            });
        }

        let mut image = vec![0xff; geometry.data_size()];
        image[..HEADER_SIZE].copy_from_slice(&geometry.header()?);
        let mut fat = vec![CHAIN_END; geometry.alloc_end as usize];
        let mut allocator = Allocator {
            next: 0,
            limit: geometry.alloc_end as usize,
        };

        let root_count = saves.len().checked_add(2).ok_or(ParseError::FileTooLarge)?;
        let root_start = allocator.take(Self::dirent_clusters(root_count)?)?;
        debug_assert_eq!(root_start, 0);
        let mut root = Vec::with_capacity(root_count * DIRENT_SIZE);
        root.extend_from_slice(&Self::dot_dirent(&[], b".", root_count, root_start)?);
        root.extend_from_slice(&Self::dot_dirent(&[], b"..", 0, root_start)?);

        for save in saves {
            let files = save.files().collect::<Vec<_>>();
            let directory_count = files.len().checked_add(2).ok_or(ParseError::FileTooLarge)?;
            let directory_start = allocator.take(Self::dirent_clusters(directory_count)?)?;
            let mut directory = Vec::with_capacity(directory_count * DIRENT_SIZE);
            directory.extend_from_slice(&Self::dot_dirent(
                save.raw_dirent(),
                b".",
                directory_count,
                directory_start,
            )?);
            directory.extend_from_slice(&Self::dot_dirent(
                save.raw_dirent(),
                b"..",
                0,
                root_start,
            )?);

            for file in files {
                let data = file.data();
                let cluster = if data.is_empty() {
                    u32::MAX as usize
                } else {
                    let start = allocator.take(data.len().div_ceil(CLUSTER_SIZE))?;
                    Self::write_run(&mut image, &mut fat, geometry, start, data);
                    start
                };
                directory.extend_from_slice(&Self::encoded_dirent(
                    file.raw_dirent(),
                    file.name_bytes(),
                    DEFAULT_FILE_MODE,
                    data.len(),
                    cluster,
                )?);
            }
            Self::write_run(&mut image, &mut fat, geometry, directory_start, &directory);
            root.extend_from_slice(&Self::encoded_dirent(
                save.raw_dirent(),
                save.name_bytes(),
                DEFAULT_DIRECTORY_MODE,
                directory_count,
                directory_start,
            )?);
        }
        Self::write_run(&mut image, &mut fat, geometry, root_start, &root);
        Self::write_fat(&mut image, geometry, &fat)?;

        if with_spare {
            Ok(Self::add_spare(&image))
        } else {
            Ok(image)
        }
    }

    pub(in crate::impls::ps2) fn formatted_superblock(
        capacity_blocks: u32,
    ) -> Result<Vec<u8>, ParseError> {
        let geometry = Geometry::new(capacity_blocks)?;
        let mut block = vec![0; ERASE_BLOCK_SIZE];
        block[..HEADER_SIZE].copy_from_slice(&geometry.header()?);
        Ok(block)
    }

    fn required_clusters(saves: &[Save<'_>]) -> Result<usize, ParseError> {
        let mut clusters =
            Self::dirent_clusters(saves.len().checked_add(2).ok_or(ParseError::FileTooLarge)?)?;
        for save in saves {
            let files = save.files();
            clusters = clusters
                .checked_add(Self::dirent_clusters(files.len() + 2)?)
                .ok_or(ParseError::FileTooLarge)?;
            for file in files {
                clusters = clusters
                    .checked_add(file.data().len().div_ceil(CLUSTER_SIZE))
                    .ok_or(ParseError::FileTooLarge)?;
            }
        }
        Ok(clusters)
    }

    pub(super) fn dirent_clusters(count: usize) -> Result<usize, ParseError> {
        count
            .checked_mul(DIRENT_SIZE)
            .map(|bytes| bytes.div_ceil(CLUSTER_SIZE))
            .ok_or(ParseError::FileTooLarge)
    }

    fn write_run(
        image: &mut [u8],
        fat: &mut [u32],
        geometry: Geometry,
        start: usize,
        bytes: &[u8],
    ) {
        let clusters = bytes.len().div_ceil(CLUSTER_SIZE).max(1);
        for step in 0..clusters {
            let cluster = start + step;
            let offset = (geometry.alloc_offset as usize + cluster) * CLUSTER_SIZE;
            image[offset..offset + CLUSTER_SIZE].fill(0);
            let source = step * CLUSTER_SIZE;
            let length = (bytes.len() - source).min(CLUSTER_SIZE);
            image[offset..offset + length].copy_from_slice(&bytes[source..source + length]);
            fat[cluster] = ALLOCATED
                | if step + 1 == clusters {
                    CHAIN_END
                } else {
                    (cluster + 1) as u32
                };
        }
    }

    fn write_fat(image: &mut [u8], geometry: Geometry, fat: &[u32]) -> Result<(), ParseError> {
        let indirect_offset = geometry.ifc_cluster as usize * CLUSTER_SIZE;
        for index in 0..geometry.fat_clusters {
            let value = geometry.ifc_cluster + 1 + index;
            let offset = indirect_offset + index as usize * FAT_ENTRY_SIZE;
            image[offset..offset + FAT_ENTRY_SIZE].copy_from_slice(&value.to_le_bytes());
        }
        let entries_per_cluster = ENTRIES_PER_CLUSTER as usize;
        for (cluster, entry) in fat.iter().enumerate() {
            let fat_cluster = geometry.ifc_cluster as usize + 1 + cluster / entries_per_cluster;
            let offset =
                fat_cluster * CLUSTER_SIZE + (cluster % entries_per_cluster) * FAT_ENTRY_SIZE;
            image[offset..offset + FAT_ENTRY_SIZE].copy_from_slice(&entry.to_le_bytes());
        }
        Ok(())
    }

    pub(super) fn encoded_dirent(
        raw: &[u8],
        name: &[u8],
        default_mode: u16,
        length: usize,
        cluster: usize,
    ) -> Result<[u8; DIRENT_SIZE], ParseError> {
        let mut bytes = [0; DIRENT_SIZE];
        let supplied_mode = raw
            .get(..2)
            .map(|mode| u16::from_le_bytes([mode[0], mode[1]]))
            .unwrap_or(0);
        if raw.len() >= DIRENT_SIZE && supplied_mode & USED_MODE != 0 {
            bytes.copy_from_slice(&raw[..DIRENT_SIZE]);
            let mode = (supplied_mode & !(FILE_MODE | DIRECTORY_MODE))
                | (default_mode & (FILE_MODE | DIRECTORY_MODE));
            bytes[..2].copy_from_slice(&mode.to_le_bytes());
        } else {
            bytes[..2].copy_from_slice(&default_mode.to_le_bytes());
            if name.len() > 32 {
                return Err(ParseError::NameTooLong(name.len()));
            }
            bytes[0x40..0x40 + name.len()].copy_from_slice(name);
        }
        let length = u32::try_from(length).map_err(|_| ParseError::ValueTooLarge(length))?;
        let cluster = u32::try_from(cluster).map_err(|_| ParseError::ValueTooLarge(cluster))?;
        bytes[4..8].copy_from_slice(&length.to_le_bytes());
        bytes[16..20].copy_from_slice(&cluster.to_le_bytes());
        Ok(bytes)
    }

    pub(super) fn dot_dirent(
        metadata: &[u8],
        name: &[u8],
        length: usize,
        cluster: usize,
    ) -> Result<[u8; DIRENT_SIZE], ParseError> {
        let mut bytes = [0; DIRENT_SIZE];
        bytes[..2].copy_from_slice(&DEFAULT_DIRECTORY_MODE.to_le_bytes());
        if metadata.len() >= DIRENT_SIZE {
            bytes[8..16].copy_from_slice(&metadata[8..16]);
            bytes[24..32].copy_from_slice(&metadata[24..32]);
        }
        let length = u32::try_from(length).map_err(|_| ParseError::ValueTooLarge(length))?;
        let cluster = u32::try_from(cluster).map_err(|_| ParseError::ValueTooLarge(cluster))?;
        bytes[4..8].copy_from_slice(&length.to_le_bytes());
        bytes[16..20].copy_from_slice(&cluster.to_le_bytes());
        bytes[0x40..0x40 + name.len()].copy_from_slice(name);
        Ok(bytes)
    }

    fn add_spare(data: &[u8]) -> Vec<u8> {
        let mut raw = Vec::with_capacity(data.len() / PAGE_SIZE * RAW_PAGE_SIZE);
        for page in data.chunks_exact(PAGE_SIZE) {
            raw.extend_from_slice(page);
            raw.extend_from_slice(&Self::spare(page));
        }
        raw
    }

    pub(super) fn spare(page: &[u8]) -> [u8; SPARE_SIZE] {
        if page.iter().all(|byte| *byte == 0xff) {
            return [0xff; SPARE_SIZE];
        }
        let mut spare = [0; SPARE_SIZE];
        for (chunk, code) in page
            .chunks_exact(ECC_CHUNK_SIZE)
            .zip(spare.chunks_exact_mut(3))
        {
            code.copy_from_slice(&Self::ecc(chunk));
        }
        spare
    }

    fn ecc(data: &[u8]) -> [u8; 3] {
        let mut column = 0x77;
        let mut line_complement = 0x7f;
        let mut line = 0x7f;
        for (index, byte) in data.iter().copied().enumerate() {
            column ^= Self::column_parity(byte);
            if byte.count_ones() & 1 != 0 {
                let index = index as u8;
                line_complement ^= !index;
                line ^= index;
            }
        }
        [column, line_complement & 0x7f, line & 0x7f]
    }

    fn column_parity(byte: u8) -> u8 {
        [0x55, 0x33, 0x0f, 0x00, 0xaa, 0xcc, 0xf0]
            .into_iter()
            .enumerate()
            .fold(0, |parity, (index, mask)| {
                parity | (((byte & mask).count_ones() as u8 & 1) << index)
            })
    }
}
