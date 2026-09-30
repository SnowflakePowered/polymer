use super::*;

impl GitVolumeStorage {
    pub(crate) fn commit_pending(
        &self,
        message: &str,
        extra_parent: Option<gix::ObjectId>,
    ) -> Result<Option<gix::ObjectId>, Error> {
        self.commit_pending_reporting(message, extra_parent, &InstallEvents::default())
    }

    /// Write blobs for pending files outside the operation lock, so large commits do not
    /// block readers. A file changed after staging is written again under the lock.
    fn stage_blobs(
        &self,
        events: &InstallEvents,
    ) -> Result<BTreeMap<String, (gix::ObjectId, fs::Metadata)>, Error> {
        let files = self
            .inner
            .working
            .read()
            .entries
            .iter()
            .filter(|(_, entry)| matches!(entry, OverlayEntry::File))
            .map(|(path, _)| {
                let source = self.checkout_file(path);
                let metadata = fs::metadata(&source).map_err(|error| Error::io(&source, error))?;
                Ok((path.clone(), source, metadata))
            })
            .collect::<Result<Vec<_>, Error>>()?;
        events.phase(
            crate::installable::InstallPhase::Finalizing,
            files.iter().map(|(_, _, metadata)| metadata.len()).sum(),
        );
        let repo = self.inner.platform.repository().to_thread_local();
        let mut staged = BTreeMap::new();
        for (path, source, metadata) in files {
            let file = fs::File::open(&source).map_err(|error| Error::io(&source, error))?;
            let id = repo
                .objects
                .write_stream(
                    gix::objs::Kind::Blob,
                    metadata.len(),
                    &mut Reporting {
                        inner: file,
                        events: events.clone(),
                    },
                )
                .map_err(|error| Error::git("write volume blob", error))?;
            staged.insert(path, (id, metadata));
        }
        Ok(staged)
    }

    fn checkout_file(&self, path: &str) -> PathBuf {
        self.inner
            .checkout
            .root()
            .join(path.replace('/', std::path::MAIN_SEPARATOR_STR))
    }

    pub(crate) fn commit_pending_reporting(
        &self,
        message: &str,
        extra_parent: Option<gix::ObjectId>,
        events: &InstallEvents,
    ) -> Result<Option<gix::ObjectId>, Error> {
        let staged = self.stage_blobs(events)?;
        let _guard = self.inner.platform.0.operation.lock();
        let reference = self.ref_name();
        let repo = self.inner.platform.repository().to_thread_local();
        let parent = Self::tip_in(&repo, &reference)?;
        let mut state = self.inner.working.write();
        if extra_parent.is_none() && state.is_empty() {
            return Ok(None);
        }
        let root = Self::root_tree(&repo, parent)?;
        let root_id = root.id;
        let mut editor = root
            .edit()
            .map_err(|error| Error::git("edit volume tree", error))?;

        for (path, entry) in &state.entries {
            let git_path = format!("files/{path}");
            match entry {
                OverlayEntry::File => {
                    let source = self.checkout_file(path);
                    let mut file =
                        fs::File::open(&source).map_err(|error| Error::io(&source, error))?;
                    let metadata = file.metadata().map_err(|error| Error::io(&source, error))?;
                    let id = match staged.get(path) {
                        Some((id, before))
                            if before.len() == metadata.len()
                                && before.modified().ok() == metadata.modified().ok() =>
                        {
                            *id
                        }
                        _ => repo
                            .objects
                            .write_stream(gix::objs::Kind::Blob, metadata.len(), &mut file)
                            .map_err(|error| Error::git("write volume blob", error))?,
                    };
                    editor
                        .upsert(git_path, gix::objs::tree::EntryKind::Blob, id)
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
        }

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
        if extra_parent.is_none() && tree == root_id {
            self.write_index(self.target_index(&repo, &root)?)?;
            *self.inner.cache_ready.lock() = true;
            *state = WorkingState::default();
            return Ok(None);
        }
        let signature = PlatformRepository::signature();
        let mut time = gix::date::parse::TimeBuf::default();
        let signature = signature.to_ref(&mut time);
        let mut parents = parent.into_iter().collect::<Vec<_>>();
        if let Some(extra_parent) = extra_parent {
            parents.push(extra_parent);
        }
        let commit = repo
            .new_commit_as(signature, signature, message, tree, parents)
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
        self.write_index(self.target_index(&repo, &committed_root)?)?;
        *self.inner.cache_ready.lock() = true;
        *state = WorkingState::default();
        Ok(Some(commit))
    }
}
