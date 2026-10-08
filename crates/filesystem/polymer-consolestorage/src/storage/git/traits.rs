use super::*;

impl Storage for GitVolumeStorage {
    fn exists(&self, path: &str) -> bool {
        self.ensure_checkout().is_ok() && self.inner.checkout.exists(path)
    }

    fn metadata(&self, path: &str) -> Result<Metadata, Error> {
        Self::validate(path)?;
        self.ensure_checkout()?;
        self.inner.checkout.metadata(path)
    }

    fn read_dir(&self, path: &str, recursive: bool) -> Result<ReadDir<'_>, Error> {
        Self::validate(path)?;
        self.ensure_checkout()?;
        self.inner.checkout.read_dir(path, recursive)
    }

    fn create_dir_all(&self, path: &str) -> Result<(), Error> {
        Self::validate(path)?;
        self.ensure_checkout()?;
        self.inner.checkout.create_dir_all(path)?;
        if path.is_empty() {
            return Ok(());
        }
        let mut working = self.inner.working.write();
        for directory in Utf8UnixPath::new(path).ancestors() {
            if directory.as_str().is_empty() {
                continue;
            }
            working
                .entries
                .insert(directory.as_str().to_owned(), OverlayEntry::Directory);
        }
        drop(working);
        Ok(())
    }

    fn create(&self, path: &str) -> Result<Arc<dyn StorageFile>, Error> {
        Self::validate(path)?;
        if path.is_empty() {
            return Err(Error::InvalidPath(path.to_owned()));
        }
        self.ensure_checkout()?;
        let file = self.inner.checkout.create(path)?;
        self.inner
            .working
            .write()
            .entries
            .insert(path.to_owned(), OverlayEntry::File);
        self.inner.changes.notify_one();
        Ok(Arc::new(GitVolumeFile::new(
            file,
            self.inner.changes.clone(),
        )))
    }

    fn open(&self, path: &str, write: bool) -> Result<Arc<dyn StorageFile>, Error> {
        Self::validate(path)?;
        self.ensure_checkout()?;
        let file = self.inner.checkout.open(path, write)?;
        if write {
            self.inner
                .working
                .write()
                .entries
                .insert(path.to_owned(), OverlayEntry::File);
        }
        if write {
            Ok(Arc::new(GitVolumeFile::new(
                file,
                self.inner.changes.clone(),
            )))
        } else {
            Ok(file)
        }
    }

    fn remove(&self, path: &str) -> Result<(), Error> {
        Self::validate(path)?;
        if path.is_empty() {
            return Err(Error::InvalidPath(path.to_owned()));
        }
        self.ensure_checkout()?;
        self.inner.checkout.remove(path)?;
        self.inner
            .working
            .write()
            .entries
            .insert(path.to_owned(), OverlayEntry::Deleted);
        self.inner.changes.notify_one();
        Ok(())
    }

    fn watch(&self) -> Result<Option<Box<dyn ChangeSignal>>, Error> {
        self.ensure_checkout()?;
        Ok(Some(Box::new(self.inner.changes.clone())))
    }

    fn install_bytes(&self, bytes: &[u8], dest: &str) -> Result<InstallResult, Error> {
        Self::validate(dest)?;
        self.ensure_checkout()?;
        let result = self.inner.checkout.install_bytes(bytes, dest)?;
        self.inner
            .working
            .write()
            .entries
            .insert(dest.to_owned(), OverlayEntry::File);
        self.inner.changes.notify_one();
        Ok(result)
    }

    fn install_file<'a>(
        &'a self,
        source: Box<dyn AsyncRead + Send + Unpin>,
        dest: &str,
    ) -> BoxFuture<'a, Result<InstallResult, Error>> {
        let dest = dest.to_owned();
        Box::pin(async move {
            Self::validate(&dest)?;
            self.ensure_checkout()?;
            let result = self.inner.checkout.install_file(source, &dest).await?;
            self.inner
                .working
                .write()
                .entries
                .insert(dest, OverlayEntry::File);
            self.inner.changes.notify_one();
            Ok(result)
        })
    }
}

impl MountableStorage for GitVolumeStorage {
    fn rename(&self, from: &str, to: &str) -> Result<(), Error> {
        Self::validate(from)?;
        Self::validate(to)?;
        self.ensure_checkout()?;
        let metadata = self.metadata(from)?;
        let descendants = if metadata.kind == EntryKind::Directory {
            self.inner
                .checkout
                .read_dir(from, true)?
                .map(|entry| (entry.path, entry.metadata.kind))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        self.inner.checkout.rename(from, to)?;
        let mut working = self.inner.working.write();
        working
            .entries
            .insert(from.to_owned(), OverlayEntry::Deleted);
        if metadata.kind == EntryKind::File {
            working.entries.insert(to.to_owned(), OverlayEntry::File);
        } else {
            working
                .entries
                .insert(to.to_owned(), OverlayEntry::Directory);
            for (path, kind) in descendants {
                let target = Utf8UnixPath::new(to)
                    .join(
                        Utf8UnixPath::new(&path)
                            .strip_prefix(from)
                            .expect("descendant begins with its requested root"),
                    )
                    .into_string();
                working.entries.insert(path, OverlayEntry::Deleted);
                working.entries.insert(
                    target,
                    match kind {
                        EntryKind::Directory => OverlayEntry::Directory,
                        EntryKind::File => OverlayEntry::File,
                    },
                );
            }
        }
        drop(working);
        self.inner.changes.notify_one();
        Ok(())
    }

    fn truncate(&self, path: &str, len: u64) -> Result<(), Error> {
        self.open(path, true)?.set_len(len)
    }

    fn sync(&self, path: &str) -> Result<(), Error> {
        self.open(path, true)?.sync()
    }

    fn finish(&self, _path: &str) -> Result<(), Error> {
        // Commits and explicit recovery define the persistent state. Unmounting
        // must leave uncommitted NAND changes available for inspection.
        Ok(())
    }
}

pub(super) struct GitVolumeFile {
    file: Arc<dyn StorageFile>,
    changes: Arc<Notify>,
}

impl GitVolumeFile {
    fn new(file: Arc<dyn StorageFile>, changes: Arc<Notify>) -> Self {
        Self { file, changes }
    }
}

impl StorageFile for GitVolumeFile {
    fn metadata(&self) -> Result<Metadata, Error> {
        self.file.metadata()
    }

    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<usize, Error> {
        self.file.read_at(offset, buffer)
    }

    fn write_at(&self, offset: u64, buffer: &[u8]) -> Result<usize, Error> {
        let written = self.file.write_at(offset, buffer)?;
        if written != 0 {
            self.changes.notify_one();
        }
        Ok(written)
    }

    fn set_len(&self, len: u64) -> Result<(), Error> {
        self.file.set_len(len)?;
        self.changes.notify_one();
        Ok(())
    }

    fn flush(&self) -> Result<(), Error> {
        self.file.flush()?;
        self.changes.notify_one();
        Ok(())
    }

    fn sync(&self) -> Result<(), Error> {
        self.file.sync()?;
        self.changes.notify_one();
        Ok(())
    }
}

impl VolumeStorage for GitVolumeStorage {
    fn is_dirty(&self) -> Result<bool, Error> {
        let _guard = self.inner.platform.0.operation.lock();
        let repo = self.inner.platform.repository().to_thread_local();
        if self.has_component_histories() {
            component::ComponentHistory::new(self, &repo).recover()?;
        }
        let tip = Self::tip_in(&repo, &self.ref_name())?;
        let root = Self::root_tree(&repo, tip)?;
        if !self.inner.working.read().is_empty() {
            return Ok(true);
        }
        Ok(!self.checkout_matches_commit(&repo, &root)?)
    }

    fn recover(&self) -> Result<(), Error> {
        if self.inner.kind != StorageKind::Nand {
            return Err(Error::Unsupported("Only NANDs support recovery".into()));
        }
        let _guard = self.inner.platform.0.operation.lock();
        let repo = self.inner.platform.repository().to_thread_local();
        if self.has_component_histories() {
            component::ComponentHistory::new(self, &repo).recover()?;
        }
        let mut state = self.inner.working.write();
        let tip = Self::tip_in(&repo, &self.ref_name())?.ok_or(Error::UnclaimedNand)?;
        self.refresh_checkout_locked(&repo, Some(tip))?;
        *state = WorkingState::default();
        Ok(())
    }

    fn label(&self) -> BoxFuture<'_, Result<Option<String>, Error>> {
        Box::pin(async move { Ok(self.volume_info_sync()?.and_then(|info| info.label)) })
    }

    fn set_label<'a>(&'a self, label: Option<&'a str>) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(async move {
            self.recover_components()?;
            let mut info = self.volume_info_sync()?.ok_or(Error::UnclaimedNand)?;
            if info.label.as_deref() == label {
                return Ok(());
            }
            info.label = label.map(str::to_owned);
            self.inner.working.write().metadata.volume = Some(info);
            self.commit_pending("Polymer volume label changed", None)?;
            Ok(())
        })
    }

    fn artifacts(&self) -> &dyn ArtifactRegistry {
        &self.artifacts
    }

    fn volumeinfo(&self) -> BoxFuture<'_, Result<Option<VolumeInfo>, Error>> {
        Box::pin(async move {
            self.recover_components()?;
            self.volume_info_sync()
        })
    }

    fn brand<'a>(
        &'a self,
        id: Uuid,
        platform: &'a str,
        label: Option<&'a str>,
    ) -> BoxFuture<'a, Result<VolumeInfo, Error>> {
        Box::pin(async move {
            if let Some(info) = self.volume_info_sync()? {
                return Ok(info);
            }
            if id != self.inner.id || platform != self.inner.platform.platform() {
                return Err(Error::HistoryConflict(
                    "volume identity does not match its ref".to_owned(),
                ));
            }
            let info = VolumeInfo {
                id,
                platform: platform.to_owned(),
                label: label.map(str::to_owned),
                created: Timestamp::from_second(Timestamp::now().as_second())?,
            };
            self.inner.working.write().metadata.volume = Some(info.clone());
            self.commit_pending("Polymer volume created", None)?;
            Ok(info)
        })
    }
}
