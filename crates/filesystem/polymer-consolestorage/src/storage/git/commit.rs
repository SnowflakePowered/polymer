use super::*;

#[derive(Clone, Copy)]
pub(crate) enum PendingScope<'a> {
    All,
    Within(&'a str),
    Except(&'a [String]),
}

impl PendingScope<'_> {
    pub(crate) fn includes(self, path: &str) -> bool {
        match self {
            Self::All => true,
            Self::Within(root) => {
                root.is_empty()
                    || path
                        .strip_prefix(root)
                        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
            }
            Self::Except(roots) => !roots.iter().any(|root| Self::Within(root).includes(path)),
        }
    }

    pub(crate) fn entries<V>(
        self,
        map: &BTreeMap<String, V>,
    ) -> impl Iterator<Item = (&String, &V)> {
        let prefix = match self {
            Self::Within(root) => root,
            Self::All | Self::Except(_) => "",
        };
        map.range::<str, _>((
            std::ops::Bound::Included(prefix),
            std::ops::Bound::Unbounded,
        ))
        .take_while(move |(path, _)| path.starts_with(prefix))
        .filter(move |(path, _)| self.includes(path))
    }
}

impl GitVolumeStorage {
    /// Write blobs for pending files outside the operation lock, so large commits do not
    /// block readers. A file written while it is staged stays pending for a later commit.
    fn stage_blobs(
        &self,
        context: &InstallContext,
        scope: PendingScope<'_>,
    ) -> Result<BTreeMap<String, (gix::ObjectId, PathActivity)>, Error> {
        let files = scope
            .entries(&self.inner.working.read().entries)
            .filter(|(_, entry)| matches!(entry, OverlayEntry::File))
            .filter_map(|(path, _)| {
                let source = self.checkout_file(path);
                match fs::metadata(&source) {
                    Ok(metadata) => Some(Ok((path.clone(), source, metadata))),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    Err(error) => Some(Err(Error::io(&source, error))),
                }
            })
            .collect::<Result<Vec<_>, Error>>()?;
        context.phase(
            crate::installable::InstallPhase::Finalizing,
            files.iter().map(|(_, _, metadata)| metadata.len()).sum(),
        );
        let repo = self.inner.platform.repository().to_thread_local();
        let mut staged = BTreeMap::new();
        for (path, source, metadata) in files {
            let before = self.inner.activity(&path);
            if !before.settled() {
                continue;
            }
            let file = fs::File::open(&source).map_err(|error| Error::io(&source, error))?;
            let id = repo.objects.write_stream(
                gix::objs::Kind::Blob,
                metadata.len(),
                &mut Reporting {
                    inner: file,
                    context: context.clone(),
                },
            );
            if self.inner.activity(&path) != before {
                continue;
            }
            let id = id.map_err(|error| Error::git("write volume blob", error))?;
            staged.insert(path, (id, before));
        }
        Ok(staged)
    }

    fn checkout_file(&self, path: &str) -> PathBuf {
        self.inner
            .checkout
            .root()
            .join(path.replace('/', std::path::MAIN_SEPARATOR_STR))
    }

    pub(crate) fn commit_pending(&self, message: &str) -> Result<Option<gix::ObjectId>, Error> {
        self.commit_scoped(message, &InstallContext::default(), PendingScope::All)
    }

    /// Commit pending files, reporting a finalizing phase through `context` while blobs are written.
    pub(crate) fn commit_scoped(
        &self,
        message: &str,
        context: &InstallContext,
        scope: PendingScope<'_>,
    ) -> Result<Option<gix::ObjectId>, Error> {
        let staged = self.stage_blobs(context, scope)?;
        let _guard = self.inner.platform.0.operation.lock();
        let reference = self.ref_name();
        let repo = self.inner.platform.repository().to_thread_local();
        if Self::tip_in(&repo, &self.kind().deletion_ref(self.id()))?.is_some() {
            return Err(Error::HistoryConflict("volume deletion is pending".into()));
        }
        let parent = Self::tip_in(&repo, &reference)?;
        if self.has_component_histories()
            && Self::tip_in(
                &repo,
                &component::ComponentHistory::new(self, &repo).journal_ref(),
            )?
            .is_some()
        {
            return Err(Error::HistoryConflict(
                "component transaction requires recovery".into(),
            ));
        }
        let mut state = self.inner.working.write();
        if state.metadata.is_empty() && scope.entries(&state.entries).next().is_none() {
            return Ok(None);
        }
        let root = Self::root_tree(&repo, parent)?;
        let root_id = root.id;
        let mut editor = root
            .edit()
            .map_err(|error| Error::git("edit volume tree", error))?;

        let mut committed = BTreeSet::new();
        for (path, entry) in scope.entries(&state.entries) {
            let git_path = format!("files/{path}");
            match entry {
                OverlayEntry::File => {
                    let Some((id, before)) = staged.get(path) else {
                        if !self.checkout_file(path).exists() {
                            committed.insert(path.clone());
                        }
                        continue;
                    };
                    if self.inner.activity(path) != *before {
                        continue;
                    }
                    editor
                        .upsert(git_path, gix::objs::tree::EntryKind::Blob, *id)
                        .map_err(|error| Error::git("update volume file", error))?;
                }
                OverlayEntry::Directory => {
                    let key = TreeKey::new(path);
                    let id = repo
                        .write_blob(path.as_bytes())
                        .map_err(|error| Error::git("write directory metadata", error))?
                        .detach();
                    editor
                        .upsert(
                            format!("meta/fs/directories/{key}"),
                            gix::objs::tree::EntryKind::Blob,
                            id,
                        )
                        .map_err(|error| Error::git("update directory metadata", error))?;
                }
                OverlayEntry::Deleted => {
                    editor
                        .remove(&git_path)
                        .map_err(|error| Error::git("remove volume path", error))?;
                    let directories = self.base_directories(&repo, &root)?;
                    for directory in directories.iter().filter(|directory| {
                        *directory == path || directory.starts_with(&format!("{path}/"))
                    }) {
                        editor
                            .remove_leaf(format!("meta/fs/directories/{}", TreeKey::new(directory)))
                            .map_err(|error| Error::git("remove directory metadata", error))?;
                    }
                }
            }
            committed.insert(path.clone());
        }
        state.entries.retain(|path, _| !committed.contains(path));
        let pending = state
            .entries
            .iter()
            .filter(|(_, entry)| matches!(entry, OverlayEntry::File))
            .map(|(path, _)| path.clone())
            .collect::<BTreeSet<_>>();

        if let Some(info) = &state.metadata.volume {
            PlatformRepository::write_text(&mut editor, "meta/volume/id", &info.id.to_string())?;
            PlatformRepository::write_text(
                &mut editor,
                "meta/volume/kind",
                self.inner.kind.metadata_name(),
            )?;
            PlatformRepository::write_text(&mut editor, "meta/volume/platform", &info.platform)?;
            PlatformRepository::write_text(
                &mut editor,
                "meta/volume/created",
                &info.created.as_second().to_string(),
            )?;
            match &info.label {
                Some(label) => {
                    PlatformRepository::write_text(&mut editor, "meta/volume/label", label)?
                }
                None => {
                    editor
                        .remove_leaf("meta/volume/label")
                        .map_err(|error| Error::git("remove volume label", error))?;
                }
            }
        }

        let replaced = state
            .metadata
            .artifacts
            .keys()
            .map(String::as_str)
            .collect();
        for stale in ArtifactRepository::new(&repo, &reference).stale_keys(&root, &replaced)? {
            editor
                .remove(&stale)
                .map_err(|error| Error::git("remove stale artifact metadata", error))?;
        }
        for (artifact_id, artifact) in &state.metadata.artifacts {
            let root = format!("meta/artifacts/{}", TreeKey::new(artifact_id));
            editor
                .remove(&root)
                .map_err(|error| Error::git("replace artifact metadata", error))?;
            if let Some(artifact) = artifact {
                artifact.write(&mut editor, &root)?;
            }
        }

        for (depot, tip) in &state.metadata.save_tips {
            let path = format!("meta/resources/saves/{depot:016x}/head");
            match tip {
                Some(tip) => PlatformRepository::write_text(&mut editor, &path, &tip.to_string())?,
                None => {
                    editor
                        .remove(&format!("meta/resources/saves/{depot:016x}"))
                        .map_err(|error| Error::git("remove profile save resource", error))?;
                }
            }
        }

        let tree = editor
            .write()
            .map_err(|error| Error::git("write volume tree", error))?
            .detach();
        if tree == root_id {
            self.write_index(self.target_index(&repo, &root)?, &pending)?;
            *self.inner.cache_ready.lock() = true;
            state.metadata = PendingMetadata::default();
            return Ok(None);
        }
        let signature = PlatformRepository::signature();
        let mut time = gix::date::parse::TimeBuf::default();
        let signature = signature.to_ref(&mut time);
        let commit = repo
            .new_commit_as(signature, signature, message, tree, parent)
            .map_err(|error| Error::git("write volume commit", error))?
            .id;
        let expected = parent.map_or(PreviousValue::MustNotExist, |id| {
            PreviousValue::ExistingMustMatch(Target::Object(id))
        });
        let edit = RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: true,
                    message: message.into(),
                },
                expected,
                new: Target::Object(commit),
            },
            name: gix::refs::FullName::try_from(reference.as_str())
                .map_err(|error| Error::git("validate volume reference", error))?,
            deref: false,
        };
        repo.edit_references_as([edit], Some(signature))
            .map_err(|error| Error::git("publish volume commit", error))?;
        let committed_root = repo
            .find_tree(tree)
            .map_err(|error| Error::git("read committed volume tree", error))?;
        fs::create_dir_all(self.inner.checkout.root())
            .map_err(|error| Error::io(self.inner.checkout.root(), error))?;
        self.write_index(self.target_index(&repo, &committed_root)?, &pending)?;
        *self.inner.cache_ready.lock() = true;
        state.metadata = PendingMetadata::default();
        Ok(Some(commit))
    }
}
