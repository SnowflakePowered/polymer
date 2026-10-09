use super::*;

mod deletion;

pub(super) struct PlatformInner {
    path: PathBuf,
    git_dir: PathBuf,
    platform: String,
    pub(super) composer: Option<&'static dyn crate::nand::composition::NandComposer>,
    repository: gix::ThreadSafeRepository,
    pub(super) operation: Arc<Mutex<()>>,
}

#[derive(Clone)]
pub struct PlatformRepository(pub(super) Arc<PlatformInner>);

impl fmt::Debug for PlatformRepository {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlatformRepository")
            .field("path", &self.0.path)
            .field("platform", &self.0.platform)
            .finish_non_exhaustive()
    }
}

impl PlatformRepository {
    pub fn open(path: impl Into<PathBuf>, platform: &str) -> Result<Self, Error> {
        let path = path.into();
        let repository = if path.join(".git").is_dir() {
            gix::open(&path).map_err(|error| Error::git("open platform repository", error))?
        } else {
            let parent = path
                .parent()
                .ok_or_else(|| Error::InvalidPath(path.display().to_string()))?;
            std::fs::create_dir_all(parent).map_err(|error| Error::io(parent, error))?;
            gix::ThreadSafeRepository::init(
                &path,
                gix::create::Kind::WithWorktree,
                gix::create::Options::default(),
            )
            .map_err(|error| Error::git("initialize platform repository", error))?
            .to_thread_local()
        };
        let repository = repository.into_sync();
        let git_dir = fs::canonicalize(repository.git_dir())
            .map_err(|error| Error::io(repository.git_dir(), error))?;
        let operation = Self::operation_lock(&git_dir);
        let guard = operation.lock();
        Self::configure(&repository, platform)?;
        drop(guard);
        let platform = Self(Arc::new(PlatformInner {
            path,
            git_dir,
            platform: platform.to_owned(),
            composer: <dyn crate::nand::composition::NandComposer>::for_platform(platform),
            repository,
            operation,
        }));
        platform.recover_deletions()?;
        Ok(platform)
    }

    fn operation_lock(path: &Path) -> Arc<Mutex<()>> {
        static LOCKS: std::sync::OnceLock<dashmap::DashMap<PathBuf, std::sync::Weak<Mutex<()>>>> =
            std::sync::OnceLock::new();
        let mut entry = LOCKS
            .get_or_init(dashmap::DashMap::new)
            .entry(path.to_owned())
            .or_default();
        if let Some(lock) = entry.upgrade() {
            return lock;
        }
        let lock = Arc::new(Mutex::new(()));
        *entry = Arc::downgrade(&lock);
        lock
    }

    pub(super) fn write_text(
        editor: &mut gix::object::tree::Editor<'_>,
        path: &str,
        value: &str,
    ) -> Result<(), Error> {
        let blob = editor
            .repo
            .write_blob(value.as_bytes())
            .map_err(|error| Error::git("write metadata value", error))?
            .detach();
        editor
            .upsert(path, gix::objs::tree::EntryKind::Blob, blob)
            .map(|_| ())
            .map_err(|error| Error::git("update metadata value", error))
    }

    pub(super) fn signature() -> gix::actor::Signature {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let time = format!("{seconds} +0000");
        gix::actor::SignatureRef {
            name: BStr::new(b"Polymer"),
            email: BStr::new(b"storage@polymer.invalid"),
            time: &time,
        }
        .to_owned()
        .expect("Polymer generates a valid Git signature")
    }

    fn configure(repository: &gix::ThreadSafeRepository, platform: &str) -> Result<(), Error> {
        let config_path = repository.git_dir().join("config");
        let mut config = std::fs::read_to_string(&config_path)
            .map_err(|error| Error::io(&config_path, error))?;
        let marker = "# polymer platform storage v2";
        let mut changed = false;
        if !config.contains(marker) {
            config.push_str(&format!(
                "\n{marker}\n[polymer]\n\tschemaVersion = {SCHEMA_VERSION}\n\tplatform = {platform}\n[core]\n\tlogAllRefUpdates = always\n"
            ));
            changed = true;
        }
        let archive_policy = "# consoles recently-deleted retention v2";
        if !config.contains(archive_policy) {
            config.push_str(&format!(
                "\n{archive_policy}\n[gc \"refs/consoles/archive/v{SCHEMA_VERSION}/*/recently-deleted\"]\n\treflogExpireUnreachable = 30 days\n"
            ));
            changed = true;
        }
        if changed {
            std::fs::write(&config_path, &config)
                .map_err(|error| Error::io(&config_path, error))?;
        }
        if !config.contains(&format!("schemaVersion = {SCHEMA_VERSION}"))
            || !config.contains(&format!("platform = {platform}"))
        {
            return Err(Error::HistoryConflict(format!(
                "{} is not a schema {SCHEMA_VERSION} {platform} platform repository",
                repository.git_dir().display()
            )));
        }
        Ok(())
    }

    pub fn platform(&self) -> &str {
        &self.0.platform
    }

    pub fn path(&self) -> &Path {
        &self.0.path
    }

    pub(crate) fn git_dir(&self) -> &Path {
        &self.0.git_dir
    }

    pub fn checkout_root(&self) -> PathBuf {
        self.0.path.join("files")
    }

    pub(super) fn checkout_path(&self, kind: StorageKind, id: Uuid) -> PathBuf {
        self.checkout_root()
            .join(kind.checkout_namespace())
            .join(id.simple().to_string())
    }

    pub fn volume(&self, kind: StorageKind, id: Uuid) -> GitVolumeStorage {
        GitVolumeStorage::new(self.clone(), kind, id)
    }

    pub fn contains(&self, kind: StorageKind, id: Uuid) -> Result<bool, Error> {
        let repo = self.0.repository.to_thread_local();
        repo.try_find_reference(&kind.ref_name(id))
            .map(|reference| reference.is_some())
            .map_err(|error| Error::git("find volume reference", error))
    }

    pub fn ids(&self, kind: StorageKind) -> Result<Vec<Uuid>, Error> {
        let repo = self.0.repository.to_thread_local();
        let namespace = match kind {
            StorageKind::Nand => "nands",
            StorageKind::SaveRepo | StorageKind::MemoryCard | StorageKind::Battery => "profiles",
        };
        let prefix = format!("refs/consoles/{namespace}/v{SCHEMA_VERSION}/");
        let mut ids = Vec::new();
        let references = repo
            .references()
            .map_err(|error| Error::git("list platform references", error))?;
        for reference in references
            .all()
            .map_err(|error| Error::git("list platform references", error))?
        {
            let reference =
                reference.map_err(|error| Error::git("read platform reference", error))?;
            let name = reference.name().as_bstr().to_str_lossy();
            if let Some(id) = name
                .strip_prefix(&prefix)
                .and_then(|id| Uuid::parse_str(id).ok())
            {
                let volume = self.volume(kind, id);
                if volume
                    .read_value("meta/volume/kind")?
                    .is_some_and(|value| value == kind.metadata_name().as_bytes())
                {
                    ids.push(id);
                }
            }
        }
        ids.sort_unstable();
        Ok(ids)
    }

    pub(crate) fn repository(&self) -> &gix::ThreadSafeRepository {
        &self.0.repository
    }

    pub fn fork_profile(
        &self,
        kind: StorageKind,
        source: Uuid,
        destination: Uuid,
        label: Option<&str>,
    ) -> Result<GitVolumeStorage, Error> {
        if kind == StorageKind::Nand {
            return Err(Error::Unsupported(
                "only save repositories, memory cards, and battery profiles can be forked"
                    .to_owned(),
            ));
        }
        let _guard = self.0.operation.lock();
        let repo = self.0.repository.to_thread_local();
        let source_ref = kind.ref_name(source);
        let destination_ref = kind.ref_name(destination);
        let source_tip = GitVolumeStorage::tip_in(&repo, &source_ref)?
            .ok_or_else(|| Error::not_found(&source_ref))?;
        if GitVolumeStorage::tip_in(&repo, &destination_ref)?.is_some() {
            return Err(Error::HistoryConflict(format!(
                "profile {destination} already exists"
            )));
        }
        let root = GitVolumeStorage::root_tree(&repo, Some(source_tip))?;
        let mut editor = root
            .edit()
            .map_err(|error| Error::git("edit forked profile tree", error))?;
        PlatformRepository::write_text(&mut editor, "meta/volume/id", &destination.to_string())?;
        PlatformRepository::write_text(
            &mut editor,
            "meta/volume/created",
            &Timestamp::now().as_second().to_string(),
        )?;
        match label {
            Some(label) => PlatformRepository::write_text(&mut editor, "meta/volume/label", label)?,
            None => {
                editor
                    .remove_leaf("meta/volume/label")
                    .map_err(|error| Error::git("remove forked profile label", error))?;
            }
        }
        let tree = editor
            .write()
            .map_err(|error| Error::git("write forked profile tree", error))?
            .detach();
        let signature = PlatformRepository::signature();
        let mut time = gix::date::parse::TimeBuf::default();
        let signature = signature.to_ref(&mut time);
        let commit = repo
            .new_commit_as(
                signature,
                signature,
                "Polymer profile fork",
                tree,
                [source_tip],
            )
            .map_err(|error| Error::git("write forked profile commit", error))?
            .id;
        let mut edits = vec![RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: true,
                    message: "polymer: fork profile".into(),
                },
                expected: PreviousValue::MustNotExist,
                new: Target::Object(commit),
            },
            name: gix::refs::FullName::try_from(destination_ref.as_str())
                .map_err(|error| Error::git("validate forked profile reference", error))?,
            deref: false,
        }];
        let source_id = source.simple().to_string();
        let destination_id = destination.simple().to_string();
        let references = repo
            .references()
            .map_err(|error| Error::git("list profile resource references", error))?;
        for reference in references
            .all()
            .map_err(|error| Error::git("list profile resource references", error))?
        {
            let reference =
                reference.map_err(|error| Error::git("read profile resource reference", error))?;
            let name = reference.name().as_bstr().to_str_lossy().into_owned();
            let is_resource = name.starts_with(&format!("refs/consoles/saves/v2/{source_id}/"))
                || name.starts_with(&format!("refs/notes/consoles/v2/{source_id}/"));
            if !is_resource {
                continue;
            }
            let Some(id) = reference.try_id().map(|id| id.detach()) else {
                continue;
            };
            let name = name.replacen(&source_id, &destination_id, 1);
            edits.push(RefEdit {
                change: Change::Update {
                    log: LogChange {
                        mode: RefLog::AndReference,
                        force_create_reflog: true,
                        message: "polymer: fork profile resource".into(),
                    },
                    expected: PreviousValue::MustNotExist,
                    new: Target::Object(id),
                },
                name: gix::refs::FullName::try_from(name.as_str())
                    .map_err(|error| Error::git("validate forked resource reference", error))?,
                deref: false,
            });
        }
        repo.edit_references_as(edits, Some(signature))
            .map_err(|error| Error::git("publish forked profile", error))?;
        Ok(self.volume(kind, destination))
    }
}
