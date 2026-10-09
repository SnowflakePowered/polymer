use super::*;

type Files = BTreeMap<String, gix::ObjectId>;

impl StorageKind {
    pub(in crate::storage::git) fn recovered_ref(self, id: Uuid) -> String {
        format!(
            "refs/consoles/recovered/v2/{}/{}",
            self.metadata_name(),
            id.simple()
        )
    }

    pub(in crate::storage::git) fn recovering_ref(self, id: Uuid) -> String {
        format!(
            "refs/consoles/recovering/v2/{}/{}",
            self.metadata_name(),
            id.simple()
        )
    }
}

impl GitVolumeStorage {
    pub(super) fn preserve_checkout(
        &self,
        repo: &gix::Repository,
        tip: Option<gix::ObjectId>,
        root: &gix::Tree<'_>,
    ) -> Result<(), Error> {
        let recovering = Self::tip_in(repo, &self.kind().recovering_ref(self.id()))?;
        let (files, directories) = self.checkout_snapshot(repo)?;
        let files = match recovering {
            Some(entry) => {
                let entry = self.tree_files(repo, &Self::root_tree(repo, Some(entry))?)?;
                let rebased =
                    Self::rebase_snapshot(entry.clone(), &self.tree_files(repo, root)?, files);
                if rebased == entry {
                    return Ok(());
                }
                rebased
            }
            None => files,
        };
        self.record_checkout(repo, tip, root, &files, &directories)
    }

    fn record_checkout(
        &self,
        repo: &gix::Repository,
        tip: Option<gix::ObjectId>,
        root: &gix::Tree<'_>,
        files: &Files,
        directories: &BTreeSet<String>,
    ) -> Result<(), Error> {
        let recovering_ref = self.kind().recovering_ref(self.id());
        let recovering = Self::tip_in(repo, &recovering_ref)?;
        let recovered_ref = self.kind().recovered_ref(self.id());
        let recovered = Self::tip_in(repo, &recovered_ref)?;
        let mut editor = root
            .edit()
            .map_err(|error| Error::git("edit recovered tree", error))?;
        for path in ["files", "meta/fs/directories"] {
            editor
                .remove(path)
                .map_err(|error| Error::git("replace recovered tree", error))?;
        }
        for (path, id) in files {
            editor
                .upsert(
                    format!("files/{path}"),
                    gix::objs::tree::EntryKind::Blob,
                    *id,
                )
                .map_err(|error| Error::git("write recovered file", error))?;
        }
        for directory in directories {
            PlatformRepository::write_text(
                &mut editor,
                &format!("meta/fs/directories/{}", TreeKey::new(directory)),
                directory,
            )?;
        }
        let tree = editor
            .write()
            .map_err(|error| Error::git("write recovered tree", error))?
            .detach();
        let signature = PlatformRepository::signature();
        let mut time = gix::date::parse::TimeBuf::default();
        let signature = signature.to_ref(&mut time);
        let commit = repo
            .new_commit_as(
                signature,
                signature,
                "Polymer crash recovery",
                tree,
                tip.into_iter().chain(recovered).collect::<Vec<_>>(),
            )
            .map_err(|error| Error::git("write recovered commit", error))?
            .id;
        let edits = [(recovered_ref, recovered), (recovering_ref, recovering)]
            .into_iter()
            .map(|(name, previous)| {
                Ok(RefEdit {
                    change: Change::Update {
                        log: LogChange {
                            mode: RefLog::AndReference,
                            force_create_reflog: true,
                            message: "Polymer crash recovery".into(),
                        },
                        expected: previous.map_or(PreviousValue::MustNotExist, |id| {
                            PreviousValue::ExistingMustMatch(Target::Object(id))
                        }),
                        new: Target::Object(commit),
                    },
                    name: gix::refs::FullName::try_from(name.as_str())
                        .map_err(|error| Error::git("validate recovery reference", error))?,
                    deref: false,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        repo.edit_references_as(edits, Some(signature))
            .map_err(|error| Error::git("publish recovered checkout", error))?;
        Ok(())
    }

    pub(super) fn recovery_owned_elsewhere(&self, repo: &gix::Repository) -> Result<bool, Error> {
        let id = self.id().simple().to_string();
        let marker = repo
            .git_dir()
            .join("polymer/operations")
            .join(&id)
            .join("pending");
        Ok(marker
            .try_exists()
            .map_err(|error| Error::io(&marker, error))?
            || Self::tip_in(repo, &format!("refs/consoles/transactions/v2/{id}"))?.is_some())
    }

    pub(super) fn finish_recovery(&self, repo: &gix::Repository) -> Result<(), Error> {
        let name = self.kind().recovering_ref(self.id());
        if let Some(reference) = repo
            .try_find_reference(&name)
            .map_err(|error| Error::git("find recovery marker", error))?
        {
            reference
                .delete()
                .map_err(|error| Error::git("clear recovery marker", error))?;
        }
        Ok(())
    }

    fn rebase_snapshot(mut entry: Files, committed: &Files, current: Files) -> Files {
        let paths = entry
            .keys()
            .chain(committed.keys())
            .chain(current.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        for path in paths {
            let now = current.get(&path);
            if now == entry.get(&path) || now == committed.get(&path) {
                continue;
            }
            match now {
                Some(id) => entry.insert(path, *id),
                None => entry.remove(&path),
            };
        }
        entry
    }

    fn tree_files(&self, repo: &gix::Repository, root: &gix::Tree<'_>) -> Result<Files, Error> {
        let index = self.target_index(repo, root)?;
        Ok(index
            .entries()
            .iter()
            .map(|entry| (entry.path(&index).to_str_lossy().into_owned(), entry.id))
            .collect())
    }

    fn checkout_snapshot(
        &self,
        repo: &gix::Repository,
    ) -> Result<(Files, BTreeSet<String>), Error> {
        let checkout = self.inner.checkout.root();
        let mut files = Files::new();
        let mut directories = BTreeSet::new();
        let mut pending = vec![checkout.to_path_buf()];
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(&directory).map_err(|error| Error::io(&directory, error))? {
                let path = entry.map_err(|error| Error::io(&directory, error))?.path();
                let relative = path
                    .strip_prefix(checkout)
                    .map_err(|_| Error::InvalidPath(path.display().to_string()))?
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/");
                Self::validate(&relative)?;
                let metadata =
                    fs::symlink_metadata(&path).map_err(|error| Error::io(&path, error))?;
                if metadata.is_dir() {
                    directories.insert(relative);
                    pending.push(path);
                    continue;
                }
                let mut file = fs::File::open(&path).map_err(|error| Error::io(&path, error))?;
                let id = repo
                    .objects
                    .write_stream(gix::objs::Kind::Blob, metadata.len(), &mut file)
                    .map_err(|error| Error::git("write recovered blob", error))?;
                files.insert(relative, id);
            }
        }
        Ok((files, directories))
    }
}
