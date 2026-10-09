use super::*;

impl<'a> RawMemoryCardImage<'a> for MemoryCard<'a> {
    type Error = ParseError;
    type Identity = Vec<u8>;
    type Save<'card> = Save<'card>;

    const BLOCK_SIZE: u64 = ERASE_BLOCK_SIZE as u64;
    const MAX_BLOCKS: u32 = MAX_BLOCKS;

    fn empty(blocks: u32) -> Result<Self, Self::Error> {
        let bytes = MemoryCard::<'static>::format(blocks, &[], true)?;
        let page_stride = Self::validate(&bytes, blocks)?;
        let mut card = Self {
            bytes: CardBytes::Owned(bytes),
            page_stride,
            free_clusters: Vec::new(),
        };
        card.free_clusters = card.scan_free_clusters()?;
        Ok(card)
    }

    fn new(bytes: &'a [u8], blocks: u32) -> Result<Self, Self::Error> {
        let page_stride = Self::validate(bytes, blocks)?;
        let mut card = Self {
            bytes: CardBytes::Borrowed(bytes),
            page_stride,
            free_clusters: Vec::new(),
        };
        card.free_clusters = card.scan_free_clusters()?;
        Ok(card)
    }

    fn into_owned(self) -> Vec<u8> {
        self.bytes.into_owned()
    }

    fn saves(&self) -> Result<Vec<Self::Save<'_>>, Self::Error> {
        let root = self.root_directory()?;
        root.entries()
            .filter(|entry| Self::is_save_entry(entry))
            .map(|entry| self.decode_save(entry))
            .collect()
    }

    fn lenient_saves(&self) -> Result<Vec<(Self::Identity, Option<Self::Save<'_>>)>, Self::Error> {
        let root = self.root_directory()?;
        Ok(root
            .entries()
            .filter(|entry| Self::is_save_entry(entry))
            .map(|entry| {
                let save = self.decode_save(entry).ok();
                let complete = save.filter(|save| save.files().len() != 0);
                (entry.name_bytes().to_vec(), complete)
            })
            .collect())
    }

    fn append_save(&mut self, save: Self::Save<'_>) -> Result<(), Self::Error> {
        if self.bytes.as_mut_slice().is_none() {
            return Err(ParseError::ReadOnly);
        }

        let superblock = self.superblock();
        let alloc_offset = superblock.alloc_offset();
        let cluster_size = superblock.cluster_size();
        let entries_per_cluster = cluster_size / DIRENT_SIZE;
        let root = self.root_directory()?;
        if root.entries().any(|entry| {
            entry.is_used() && entry.is_directory() && entry.name_bytes() == save.name_bytes()
        }) {
            return Err(ParseError::DuplicateSave(save.name().into_owned()));
        }

        let root_count = root.count;
        let mut root_clusters = root.clusters.clone();
        let root_cluster_count = (root_count + 1)
            .checked_mul(DIRENT_SIZE)
            .ok_or(ParseError::FileTooLarge)?
            .div_ceil(cluster_size);
        let root_extra = root_cluster_count - root_clusters.len();
        let files = save.files().collect::<Vec<_>>();
        let directory_count = files.len().checked_add(2).ok_or(ParseError::FileTooLarge)?;
        let directory_clusters = MemoryCard::<'static>::dirent_clusters(directory_count)?;
        let file_cluster_counts = files
            .iter()
            .map(|file| file.data().len().div_ceil(cluster_size))
            .collect::<Vec<_>>();
        let needed = root_extra
            .checked_add(directory_clusters)
            .and_then(|needed| {
                file_cluster_counts
                    .iter()
                    .try_fold(needed, |needed, count| needed.checked_add(*count))
            })
            .ok_or(ParseError::FileTooLarge)?;
        if needed > self.free_clusters.len() {
            return Err(ParseError::Full {
                needed,
                available: self.free_clusters.len(),
            });
        }

        let allocated = &self.free_clusters[..needed];
        let mut cursor = 0;
        let new_root_clusters = allocated[cursor..cursor + root_extra].to_vec();
        cursor += root_extra;
        let save_directory_clusters = allocated[cursor..cursor + directory_clusters].to_vec();
        cursor += directory_clusters;
        let mut file_clusters = Vec::with_capacity(files.len());
        for count in file_cluster_counts {
            file_clusters.push(allocated[cursor..cursor + count].to_vec());
            cursor += count;
        }
        debug_assert_eq!(cursor, needed);

        let old_root_last = *root_clusters
            .last()
            .expect("the validated root directory has a cluster");
        root_clusters.extend_from_slice(&new_root_clusters);
        let save_directory_start = save_directory_clusters[0];
        let mut directory = Vec::with_capacity(directory_count * DIRENT_SIZE);
        directory.extend_from_slice(&MemoryCard::<'static>::dot_dirent(
            save.raw_dirent(),
            b".",
            directory_count,
            save_directory_start,
        )?);
        directory.extend_from_slice(&MemoryCard::<'static>::dot_dirent(
            save.raw_dirent(),
            b"..",
            0,
            root_clusters[0],
        )?);
        for (file, clusters) in files.iter().zip(&file_clusters) {
            let first_cluster = clusters.first().copied().unwrap_or(u32::MAX as usize);
            directory.extend_from_slice(&MemoryCard::<'static>::encoded_dirent(
                file.raw_dirent(),
                file.name_bytes(),
                DEFAULT_FILE_MODE,
                file.data().len(),
                first_cluster,
            )?);
        }
        let root_entry = MemoryCard::<'static>::encoded_dirent(
            save.raw_dirent(),
            save.name_bytes(),
            DEFAULT_DIRECTORY_MODE,
            directory_count,
            save_directory_start,
        )?;

        let mut fat_updates = Vec::new();
        if let Some(first) = new_root_clusters.first() {
            fat_updates.push((old_root_last, ALLOCATED | *first as u32));
            Self::extend_fat_updates(&mut fat_updates, &new_root_clusters);
        }
        Self::extend_fat_updates(&mut fat_updates, &save_directory_clusters);
        for clusters in &file_clusters {
            Self::extend_fat_updates(&mut fat_updates, clusters);
        }
        let fat_writes = fat_updates
            .into_iter()
            .map(|(cluster, value)| Ok((self.fat_entry_offset(cluster)?, value)))
            .collect::<Result<Vec<_>, ParseError>>()?;

        drop(root);
        for (file, clusters) in files.iter().zip(&file_clusters) {
            self.write_chain(clusters, file.data(), alloc_offset, cluster_size)?;
        }
        self.write_chain(
            &save_directory_clusters,
            &directory,
            alloc_offset,
            cluster_size,
        )?;
        let zero_cluster = vec![0; cluster_size];
        for cluster in &new_root_clusters {
            self.write_data((alloc_offset + *cluster) * cluster_size, &zero_cluster)?;
        }
        for (offset, value) in fat_writes {
            self.write_data(offset, &value.to_le_bytes())?;
        }

        let root_entry_cluster = root_clusters[root_count / entries_per_cluster];
        let root_entry_offset = (alloc_offset + root_entry_cluster) * cluster_size
            + (root_count % entries_per_cluster) * DIRENT_SIZE;
        self.write_data(root_entry_offset, &root_entry)?;
        let root_length_offset = (alloc_offset + root_clusters[0]) * cluster_size + 4;
        let new_root_count =
            u32::try_from(root_count + 1).map_err(|_| ParseError::ValueTooLarge(root_count + 1))?;
        self.write_data(root_length_offset, &new_root_count.to_le_bytes())?;

        self.free_clusters.drain(..needed);
        Ok(())
    }
}

impl MemoryCard<'_> {
    fn extend_fat_updates(updates: &mut Vec<(usize, u32)>, clusters: &[usize]) {
        for (index, cluster) in clusters.iter().copied().enumerate() {
            let next = clusters
                .get(index + 1)
                .map_or(CHAIN_END, |next| *next as u32);
            updates.push((cluster, ALLOCATED | next));
        }
    }

    fn fat_entry_offset(&self, cluster: usize) -> Result<usize, ParseError> {
        let superblock = self.superblock();
        let entries_per_cluster = superblock.entries_per_cluster();
        let indirect_index = cluster / entries_per_cluster;
        let indirect_slot = indirect_index / entries_per_cluster;
        let indirect_cluster = superblock
            .indirect_fat_cluster(indirect_slot)
            .ok_or(ParseError::MissingIndirectFatCluster(indirect_slot))?;
        let fat_cluster =
            self.raw_cluster_u32(indirect_cluster, indirect_index % entries_per_cluster)? as usize;
        if fat_cluster == 0 || fat_cluster >= superblock.alloc_offset() {
            return Err(ParseError::InvalidFatCluster {
                index: indirect_index,
                cluster: fat_cluster,
            });
        }
        Ok(fat_cluster * superblock.cluster_size()
            + (cluster % entries_per_cluster) * FAT_ENTRY_SIZE)
    }

    fn write_chain(
        &mut self,
        clusters: &[usize],
        data: &[u8],
        alloc_offset: usize,
        cluster_size: usize,
    ) -> Result<(), ParseError> {
        let zero_cluster = vec![0; cluster_size];
        for (index, cluster) in clusters.iter().copied().enumerate() {
            let offset = (alloc_offset + cluster) * cluster_size;
            self.write_data(offset, &zero_cluster)?;
            let source = index * cluster_size;
            let length = data.len().saturating_sub(source).min(cluster_size);
            if length != 0 {
                self.write_data(offset, &data[source..source + length])?;
            }
        }
        Ok(())
    }

    fn write_data(&mut self, mut offset: usize, mut data: &[u8]) -> Result<(), ParseError> {
        let page_len = self.superblock().page_len();
        let page_stride = self.page_stride;
        let bytes = self.bytes.as_mut_slice().ok_or(ParseError::ReadOnly)?;
        while !data.is_empty() {
            let page = offset / page_len;
            let within_page = offset % page_len;
            let length = data.len().min(page_len - within_page);
            let physical = page
                .checked_mul(page_stride)
                .and_then(|offset| offset.checked_add(within_page))
                .ok_or(ParseError::InvalidCardOffset(offset))?;
            bytes
                .get_mut(physical..physical + length)
                .ok_or(ParseError::InvalidCardOffset(offset))?
                .copy_from_slice(&data[..length]);

            if page_stride != page_len {
                let page_offset = page * page_stride;
                let spare = MemoryCard::spare(&bytes[page_offset..page_offset + page_len]);
                bytes[page_offset + page_len..page_offset + page_stride].copy_from_slice(&spare);
            }
            offset += length;
            data = &data[length..];
        }
        Ok(())
    }

    fn is_save_entry(entry: &Dirent) -> bool {
        entry.is_used() && entry.is_directory() && !matches!(entry.name_bytes(), b"." | b"..")
    }

    fn decode_save(&self, entry: &Dirent) -> Result<Save<'static>, ParseError> {
        let directory = self.directory(entry)?;
        let files = directory
            .entries()
            .filter(|file| file.is_used() && !matches!(file.name_bytes(), b"." | b".."))
            .collect::<Vec<_>>();
        if files.iter().any(|file| !file.is_file()) {
            return Err(ParseError::InvalidDirectory(
                "save directory contains a non-file entry",
            ));
        }
        let mut encoder = Encoder::new(entry.as_bytes(), files.len())?;
        for file in files {
            encoder.file(file.as_bytes(), file.length(), self.file_chunks(file)?)?;
        }
        Ok(encoder.finish()?)
    }
}
