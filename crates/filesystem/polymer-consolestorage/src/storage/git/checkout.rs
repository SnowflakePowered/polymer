use super::*;

impl GitVolumeStorage {
    fn files_tree_id(repo: &gix::Repository, root: &gix::Tree<'_>) -> Result<gix::ObjectId, Error> {
        match root
            .lookup_entry_by_path("files")
            .map_err(|error| Error::git("lookup checked-out files tree", error))?
        {
            Some(entry) if entry.mode().kind() == gix::objs::tree::EntryKind::Tree => {
                Ok(entry.object_id())
            }
            Some(_) => Err(Error::HistoryConflict(
                "volume files root is not a tree".to_owned(),
            )),
            None => Ok(repo.empty_tree().id),
        }
    }

    pub(super) fn target_index(
        &self,
        repo: &gix::Repository,
        root: &gix::Tree<'_>,
    ) -> Result<gix::index::File, Error> {
        let tree = Self::files_tree_id(repo, root)?;
        repo.index_from_tree(&tree)
            .map_err(|error| Error::git("build volume checkout index", error))
    }

    fn index_prefix(&self) -> String {
        format!(
            "files/{}/{}/",
            self.inner.kind.checkout_namespace(),
            self.inner.id.simple()
        )
    }

    fn volume_index_matches(&self, platform: &gix::index::File, target: &gix::index::File) -> bool {
        let prefix = self.index_prefix();
        let current = platform
            .entries()
            .iter()
            .filter_map(|entry| {
                entry
                    .path(platform)
                    .to_str_lossy()
                    .strip_prefix(&prefix)
                    .map(|path| (path.to_owned(), entry))
            })
            .collect::<Vec<_>>();
        current.len() == target.entries().len()
            && current
                .iter()
                .zip(target.entries())
                .all(|((path, current), wanted)| {
                    path.as_bytes() == wanted.path(target).as_bytes()
                        && current.id == wanted.id
                        && current.mode == wanted.mode
                })
    }

    fn checkout_is_complete(&self, index: &gix::index::File) -> bool {
        self.inner.checkout.root().is_dir()
            && index.entries().iter().all(|entry| {
                let path = entry.path(index).to_str_lossy();
                Self::validate(&path).is_ok()
                    && self
                        .inner
                        .checkout
                        .root()
                        .join(path.replace('/', std::path::MAIN_SEPARATOR_STR))
                        .is_file()
            })
    }

    pub(super) fn write_index(&self, target: gix::index::File) -> Result<(), Error> {
        let parent = self
            .inner
            .index_path
            .parent()
            .ok_or_else(|| Error::InvalidPath(self.inner.index_path.display().to_string()))?;
        fs::create_dir_all(parent).map_err(|error| Error::io(parent, error))?;
        let mut index = if self.inner.index_path.is_file() {
            gix::index::File::at(
                &self.inner.index_path,
                target.object_hash(),
                false,
                gix::index::decode::Options::default(),
            )
            .map_err(|error| Error::git("read platform checkout index", error))?
        } else {
            gix::index::File::from_state(
                gix::index::State::new(target.object_hash()),
                &self.inner.index_path,
            )
        };
        let prefix = self.index_prefix();
        index.remove_entries(|_, path, _| path.to_str_lossy().starts_with(&prefix));
        for entry in target.entries() {
            let relative = entry.path(&target).to_str_lossy();
            Self::validate(&relative)?;
            let path = self
                .inner
                .checkout
                .root()
                .join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
            let metadata = gix::index::fs::Metadata::from_path_no_follow(&path)
                .map_err(|error| Error::io(&path, error))?;
            let stat = gix::index::entry::Stat::from_fs(&metadata)
                .map_err(|error| Error::git("record checkout file metadata", error))?;
            let path = format!("{prefix}{relative}");
            index.dangerously_push_entry(
                stat,
                entry.id,
                entry.flags,
                entry.mode,
                BStr::new(path.as_bytes()),
            );
        }
        index.remove_tree();
        index.sort_entries();
        index.set_path(&self.inner.index_path);
        index
            .write(gix::index::write::Options::default())
            .map_err(|error| Error::git("write volume checkout index", error))
    }

    pub(super) fn remove_from_index(&self, object_hash: gix::hash::Kind) -> Result<(), Error> {
        if !self.inner.index_path.is_file() {
            return Ok(());
        }
        let mut index = gix::index::File::at(
            &self.inner.index_path,
            object_hash,
            false,
            gix::index::decode::Options::default(),
        )
        .map_err(|error| Error::git("read platform checkout index", error))?;
        let prefix = self.index_prefix();
        index.remove_entries(|_, path, _| path.to_str_lossy().starts_with(&prefix));
        index.remove_tree();
        index.set_path(&self.inner.index_path);
        index
            .write(gix::index::write::Options::default())
            .map_err(|error| Error::git("remove volume from checkout index", error))
    }

    fn checkout_target(
        &self,
        repo: &gix::Repository,
        root: &gix::Tree<'_>,
        index: &gix::index::File,
    ) -> Result<(), Error> {
        let checkout = self.inner.checkout.root();
        fs::create_dir_all(checkout).map_err(|error| Error::io(checkout, error))?;
        let indexed = self.indexed_checkout(repo.object_hash())?;
        let mut expected = BTreeSet::new();
        for entry in index.entries() {
            let relative = entry.path(index).to_str_lossy();
            Self::validate(&relative)?;
            let path = PathBuf::from(relative.as_ref());
            let destination = checkout.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
            let current = indexed.get(&path).is_some_and(|(stat, id)| {
                *id == entry.id && Self::stat_matches(stat, &destination)
            });
            expected.insert(path);
            if current {
                continue;
            }
            if destination.is_dir() {
                fs::remove_dir_all(&destination).map_err(|error| Error::io(&destination, error))?;
            }
            let parent = destination
                .parent()
                .ok_or_else(|| Error::InvalidPath(destination.display().to_string()))?;
            fs::create_dir_all(parent).map_err(|error| Error::io(parent, error))?;
            let blob = repo
                .find_blob(entry.id)
                .map_err(|error| Error::git("read checkout blob", error))?;
            let mut file =
                fs::File::create(&destination).map_err(|error| Error::io(&destination, error))?;
            file.write_all(&blob.data)
                .map_err(|error| Error::io(&destination, error))?;
        }

        for directory in self.base_directories(repo, root)? {
            Self::validate(&directory)?;
            let destination = checkout.join(directory.replace('/', std::path::MAIN_SEPARATOR_STR));
            fs::create_dir_all(&destination).map_err(|error| Error::io(&destination, error))?;
            expected.insert(PathBuf::from(directory));
        }
        for path in expected.iter().cloned().collect::<Vec<_>>() {
            expected.extend(
                path.ancestors()
                    .skip(1)
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .map(Path::to_path_buf),
            );
        }
        Self::prune_checkout(checkout, checkout, &expected)
    }

    fn prune_checkout(
        root: &Path,
        directory: &Path,
        expected: &BTreeSet<PathBuf>,
    ) -> Result<(), Error> {
        for entry in fs::read_dir(directory).map_err(|error| Error::io(directory, error))? {
            let path = entry.map_err(|error| Error::io(directory, error))?.path();
            let relative = path
                .strip_prefix(root)
                .map_err(|_| Error::InvalidPath(path.display().to_string()))?;
            let metadata = fs::symlink_metadata(&path).map_err(|error| Error::io(&path, error))?;
            if expected.contains(relative) {
                if metadata.is_dir() {
                    Self::prune_checkout(root, &path, expected)?;
                }
                continue;
            }
            if metadata.is_dir() {
                fs::remove_dir_all(&path).map_err(|error| Error::io(&path, error))?;
            } else {
                fs::remove_file(&path).map_err(|error| Error::io(&path, error))?;
            }
        }
        Ok(())
    }

    fn stat_matches(stat: &gix::index::entry::Stat, path: &Path) -> bool {
        gix::index::fs::Metadata::from_path_no_follow(path)
            .ok()
            .and_then(|metadata| gix::index::entry::Stat::from_fs(&metadata).ok())
            .is_some_and(|current| stat.matches(&current, CHECKOUT_STAT))
    }

    pub(super) fn refresh_checkout_locked(
        &self,
        repo: &gix::Repository,
        tip: Option<gix::ObjectId>,
    ) -> Result<(), Error> {
        let root = Self::root_tree(repo, tip)?;
        let index = self.target_index(repo, &root)?;
        self.checkout_target(repo, &root, &index)?;
        self.write_index(index)?;
        *self.inner.cache_ready.lock() = true;
        Ok(())
    }

    fn committed_checkout_entries(
        &self,
        repo: &gix::Repository,
        root: &gix::Tree<'_>,
    ) -> Result<BTreeMap<PathBuf, Option<gix::ObjectId>>, Error> {
        let index = self.target_index(repo, root)?;
        let mut entries = BTreeMap::new();
        for entry in index.entries() {
            let path = entry.path(&index).to_str_lossy();
            Self::validate(&path)?;
            entries.insert(PathBuf::from(path.as_ref()), Some(entry.id));
        }
        for directory in self.base_directories(repo, root)? {
            Self::validate(&directory)?;
            entries.insert(PathBuf::from(directory), None);
        }
        // Git does not store implicit parent directories as separate entries.
        for path in entries.keys().cloned().collect::<Vec<_>>() {
            for parent in path
                .ancestors()
                .skip(1)
                .filter(|path| !path.as_os_str().is_empty())
            {
                entries.entry(parent.to_path_buf()).or_insert(None);
            }
        }
        Ok(entries)
    }

    pub(super) fn checkout_matches_commit(
        &self,
        repo: &gix::Repository,
        root: &gix::Tree<'_>,
    ) -> Result<bool, Error> {
        let checkout = self.inner.checkout.root();
        match fs::symlink_metadata(checkout) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(Error::io(checkout, error)),
        }
        let mut expected = self.committed_checkout_entries(repo, root)?;
        let indexed = self.indexed_checkout(repo.object_hash())?;
        let mut directories = vec![checkout.to_path_buf()];
        while let Some(directory) = directories.pop() {
            for entry in fs::read_dir(&directory).map_err(|error| Error::io(&directory, error))? {
                let entry = entry.map_err(|error| Error::io(&directory, error))?;
                let path = entry.path();
                let relative = path
                    .strip_prefix(checkout)
                    .map_err(|_| Error::InvalidPath(path.display().to_string()))?;
                let Some(object) = expected.remove(relative) else {
                    return Ok(false);
                };
                let unchanged = object.is_some_and(|object| {
                    indexed
                        .get(relative)
                        .is_some_and(|(stat, id)| *id == object && Self::stat_matches(stat, &path))
                });
                if !unchanged && !Self::checkout_entry_matches(repo, &path, object)? {
                    return Ok(false);
                }
                if object.is_none() {
                    directories.push(path);
                }
            }
        }
        Ok(expected.is_empty())
    }

    fn indexed_checkout(
        &self,
        object_hash: gix::hash::Kind,
    ) -> Result<BTreeMap<PathBuf, (gix::index::entry::Stat, gix::ObjectId)>, Error> {
        if !self.inner.index_path.is_file() {
            return Ok(BTreeMap::new());
        }
        let index = gix::index::File::at(
            &self.inner.index_path,
            object_hash,
            false,
            gix::index::decode::Options::default(),
        )
        .map_err(|error| Error::git("read platform checkout index", error))?;
        let prefix = self.index_prefix();
        Ok(index
            .entries()
            .iter()
            .filter_map(|entry| {
                let path = entry.path(&index).to_str_lossy();
                let relative = path.strip_prefix(&prefix)?;
                Some((PathBuf::from(relative), (entry.stat, entry.id)))
            })
            .collect())
    }

    fn checkout_entry_matches(
        repo: &gix::Repository,
        path: &Path,
        object: Option<gix::ObjectId>,
    ) -> Result<bool, Error> {
        let metadata = fs::symlink_metadata(path).map_err(|error| Error::io(path, error))?;
        if metadata.file_type().is_symlink() {
            return Ok(false);
        }
        let Some(object) = object else {
            return Ok(metadata.is_dir());
        };
        if !metadata.is_file() {
            return Ok(false);
        }
        let mut file = fs::File::open(path).map_err(|error| Error::io(path, error))?;
        let hash = gix::objs::compute_stream_hash(
            repo.object_hash(),
            gix::objs::Kind::Blob,
            &mut file,
            metadata.len(),
            &mut gix::progress::Discard,
            &std::sync::atomic::AtomicBool::new(false),
        )
        .map_err(|error| Error::git("hash NAND checkout file", error))?;
        Ok(hash == object)
    }

    pub(super) fn ensure_checkout(&self) -> Result<(), Error> {
        if *self.inner.cache_ready.lock() {
            return Ok(());
        }
        let _guard = self.inner.platform.0.operation.lock();
        let mut ready = self.inner.cache_ready.lock();
        if *ready {
            return Ok(());
        }
        let repo = self.inner.platform.repository().to_thread_local();
        let tip = Self::tip_in(&repo, &self.ref_name())?;
        // Existing NAND checkouts are inspected by native installation checks and repaired only
        // through recover(). Opening one must never erase an interrupted write.
        if self.inner.kind == StorageKind::Nand && tip.is_some() {
            *ready = true;
            return Ok(());
        }
        let root = Self::root_tree(&repo, tip)?;
        let target = self.target_index(&repo, &root)?;
        let current = gix::index::File::at(
            &self.inner.index_path,
            repo.object_hash(),
            false,
            gix::index::decode::Options::default(),
        );
        let reusable = current.as_ref().is_ok_and(|current| {
            self.volume_index_matches(current, &target) && self.checkout_is_complete(&target)
        });
        if !reusable {
            self.checkout_target(&repo, &root, &target)?;
            self.write_index(target)?;
        }
        *ready = true;
        Ok(())
    }

    pub(super) fn tip_in(
        repo: &gix::Repository,
        reference: &str,
    ) -> Result<Option<gix::ObjectId>, Error> {
        repo.try_find_reference(reference)
            .map_err(|error| Error::git("find volume reference", error))
            .map(|reference| {
                reference.and_then(|reference| reference.try_id().map(|id| id.detach()))
            })
    }

    pub(super) fn root_tree(
        repo: &gix::Repository,
        tip: Option<gix::ObjectId>,
    ) -> Result<gix::Tree<'_>, Error> {
        match tip {
            Some(tip) => repo
                .find_commit(tip)
                .map_err(|error| Error::git("find volume commit", error))?
                .tree()
                .map_err(|error| Error::git("read volume tree", error)),
            None => Ok(repo.empty_tree()),
        }
    }

    pub(super) fn base_directories(
        &self,
        repo: &gix::Repository,
        root: &gix::Tree<'_>,
    ) -> Result<BTreeSet<String>, Error> {
        let Some(entry) = root
            .lookup_entry_by_path("meta/fs/directories")
            .map_err(|error| Error::git("lookup directory metadata", error))?
        else {
            return Ok(BTreeSet::new());
        };
        if entry.mode().kind() != gix::objs::tree::EntryKind::Tree {
            return Err(Error::HistoryConflict(
                "directory metadata is not a tree".to_owned(),
            ));
        }
        let tree = repo
            .find_tree(entry.oid())
            .map_err(|error| Error::git("read directory metadata", error))?;
        let mut directories = BTreeSet::new();
        for entry in tree.iter() {
            let entry = entry.map_err(|error| Error::git("decode directory metadata", error))?;
            if entry.kind() != gix::objs::tree::EntryKind::Blob {
                continue;
            }
            let blob = repo
                .find_blob(entry.oid())
                .map_err(|error| Error::git("read directory path", error))?;
            directories.insert(
                String::from_utf8(blob.data.clone())
                    .map_err(|error| Error::git("decode directory path", error))?,
            );
        }
        Ok(directories)
    }
}
