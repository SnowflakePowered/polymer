use super::{FinalizationReason, Resource, ResourceLease, namespace_file::NamespaceFile};
use crate::{
    CompiledFilesystem, CreateOptions, DirectoryEntry, DirectoryNode, FileNode, MemoryFileFactory,
    Metadata, MetadataChanges, Node, OpenFile, OpenOptions, Permissions, RemoveKind,
    ResourceFactory, VfsComponent, VfsError, VfsPath, VfsPathBuf, VfsPathUnit, VfsResult, Volume,
};
use parking_lot::Mutex;
use std::{collections::HashMap, sync::Arc};

pub struct PreparedFilesystem {
    compiled: CompiledFilesystem,
    materialized: Mutex<Option<MaterializedResources>>,
}

impl PreparedFilesystem {
    pub(crate) fn new(compiled: CompiledFilesystem) -> Self {
        Self {
            compiled,
            materialized: Mutex::new(None),
        }
    }

    pub fn compiled(&self) -> &CompiledFilesystem {
        &self.compiled
    }

    pub fn materialize(&self) -> VfsResult<()> {
        let mut state = self.materialized.lock();
        if state.is_some() {
            return Ok(());
        }
        let mut volumes = HashMap::new();
        let mut files = HashMap::new();
        let mut storage_mounts = HashMap::new();
        let mut leases = Vec::new();
        for binding in self.compiled.bindings() {
            match binding.kind() {
                crate::BindingKind::Tree(resource)
                | crate::BindingKind::Storage { resource, .. } => {
                    let key = resource_key(resource);
                    if let std::collections::hash_map::Entry::Vacant(entry) = volumes.entry(key) {
                        let value = resource.materialize()?;
                        leases.push(value.lease.clone());
                        entry.insert(value.backend);
                    }
                }
                crate::BindingKind::File(resource) => {
                    let key = resource_key(resource);
                    if let std::collections::hash_map::Entry::Vacant(entry) = files.entry(key) {
                        let value = resource.materialize()?;
                        leases.push(value.lease.clone());
                        entry.insert(Arc::new(NamespaceFile::new(value.backend)));
                    }
                }
            }
        }
        for binding in self.compiled.bindings() {
            let crate::BindingKind::Storage { resource, format } = binding.kind() else {
                continue;
            };
            let volume = volumes
                .get(&resource_key(resource))
                .cloned()
                .ok_or(VfsError::StaleNode)?;
            let mounted = volume.mount(format.as_deref())?;
            storage_mounts.insert(
                binding.at().normalized_components()?,
                MaterializedStorageMount::new(mounted, volume, format.as_deref())?,
            );
        }
        *state = Some(MaterializedResources {
            volumes,
            files,
            storage_mounts,
            tree_files: HashMap::new(),
            leases,
            finalized: false,
        });
        Ok(())
    }

    pub fn finalize(&self, reason: FinalizationReason) -> VfsResult<()> {
        if let Some(resources) = self.materialized.lock().as_mut() {
            resources.finalize(reason)
        } else {
            Ok(())
        }
    }

    /// Atomically changes the file backing an existing direct file mount.
    /// Namespace lookups after this call see the replacement, while handles
    /// opened before it retain their original file object.
    pub fn replace_mounted_file(&self, path: &VfsPathBuf, contents: Vec<u8>) -> VfsResult<()> {
        self.materialize()?;
        let binding = self
            .compiled
            .resolve(path)?
            .filter(|binding| binding.at() == path)
            .ok_or_else(|| VfsError::NotFound(path.clone()))?;
        let crate::BindingKind::File(resource) = binding.kind() else {
            return Err(VfsError::Unsupported(format!(
                "{} is not a direct file mount",
                path.to_string_lossy()
            )));
        };
        let factory = if binding.permissions().can_write() {
            MemoryFileFactory::writable(contents)
        } else {
            MemoryFileFactory::immutable(contents)
        };
        let replacement = factory.materialize()?;
        let mut state = self.materialized.lock();
        let resources = state.as_mut().ok_or_else(not_materialized)?;
        resources.leases.push(replacement.lease);
        resources.files.insert(
            resource_key(resource),
            Arc::new(NamespaceFile::new(replacement.backend)),
        );
        Ok(())
    }

    pub fn metadata(&self, path: &VfsPathBuf) -> VfsResult<Metadata> {
        self.resolve_node(path)?.node.metadata()
    }

    pub fn set_metadata(&self, path: &VfsPathBuf, changes: MetadataChanges) -> VfsResult<()> {
        let resolved = self.resolve_node(path)?;
        if !resolved.permissions.can_write() {
            return Err(VfsError::PermissionDenied(path.clone()));
        }
        match resolved.node {
            Node::File(file) => file.set_metadata(changes),
            Node::Directory(directory) => directory.set_metadata(changes),
            Node::SyntheticDirectory => Err(VfsError::PermissionDenied(path.clone())),
        }
    }

    pub fn open(&self, path: &VfsPathBuf, options: OpenOptions) -> VfsResult<Box<dyn OpenFile>> {
        let resolved = self.resolve_node(path)?;
        self.open_resolved(path, resolved, options)
    }

    /// Resolves a namespace path once and returns both its metadata and, for files,
    /// an open handle. Files report metadata through the captured handle so callers
    /// do not have to repeat the namespace walk after opening.
    pub fn open_with_metadata(
        &self,
        path: &VfsPathBuf,
        options: OpenOptions,
    ) -> VfsResult<(Metadata, Option<Box<dyn OpenFile>>)> {
        let resolved = self.resolve_node(path)?;
        if (options.read && !resolved.permissions.can_read())
            || ((options.write || options.truncate) && !resolved.permissions.can_write())
        {
            return Err(VfsError::PermissionDenied(path.clone()));
        }
        match resolved.node {
            Node::File(file) => {
                let handle = file.open(options)?;
                let metadata = handle.metadata()?;
                Ok((metadata, Some(handle)))
            }
            Node::Directory(directory) => Ok((directory.metadata()?, None)),
            Node::SyntheticDirectory => Ok((
                Metadata {
                    kind: crate::FileKind::Directory,
                    len: 0,
                    readonly: true,
                    sparse: false,
                },
                None,
            )),
        }
    }

    fn open_resolved(
        &self,
        path: &VfsPathBuf,
        resolved: ResolvedNode,
        options: OpenOptions,
    ) -> VfsResult<Box<dyn OpenFile>> {
        if (options.read && !resolved.permissions.can_read())
            || ((options.write || options.truncate) && !resolved.permissions.can_write())
        {
            return Err(VfsError::PermissionDenied(path.clone()));
        }
        match resolved.node {
            Node::File(file) => file.open(options),
            Node::Directory(_) | Node::SyntheticDirectory => {
                Err(VfsError::IsDirectory(path.clone()))
            }
        }
    }

    pub fn read_dir(&self, path: &VfsPathBuf) -> VfsResult<Vec<DirectoryEntry>> {
        let resolved = self.resolve_node(path)?;
        if !resolved.permissions.can_read() || !resolved.permissions.can_execute() {
            return Err(VfsError::PermissionDenied(path.clone()));
        }
        let mut entries = std::collections::BTreeMap::<Vec<VfsPathUnit>, DirectoryEntry>::new();
        match resolved.node {
            Node::SyntheticDirectory => {}
            Node::File(_) => return Err(VfsError::NotDirectory(path.clone())),
            Node::Directory(directory) => {
                for entry in directory.read_dir()? {
                    let entry = entry?;
                    entries.insert(entry.name.normalized()?, entry);
                }
            }
        }
        for (name, _) in self.compiled.mount_children(path)? {
            let child_path = path.as_path().join(&name);
            let node = self.resolve_node(&child_path)?.node;
            let metadata = node.metadata()?;
            entries.insert(name.normalized()?, DirectoryEntry { name, metadata });
        }
        Ok(entries.into_values().collect())
    }

    pub fn create_file(&self, path: &VfsPathBuf, options: CreateOptions) -> VfsResult<()> {
        if self.compiled.is_mount_point(path)? {
            return Err(VfsError::AlreadyExists(path.clone()));
        }
        let parent = self.writable_parent(path)?;
        let file = parent.directory.create_file(&parent.name, options)?;
        self.remember_tree_file(path, file)?;
        Ok(())
    }

    pub fn create_dir(&self, path: &VfsPathBuf) -> VfsResult<()> {
        if self.compiled.is_mount_point(path)? {
            return Err(VfsError::AlreadyExists(path.clone()));
        }
        let parent = self.writable_parent(path)?;
        parent.directory.create_dir(&parent.name)?;
        Ok(())
    }

    pub fn remove(&self, path: &VfsPathBuf, kind: RemoveKind) -> VfsResult<()> {
        self.validate_remove(path, kind)?;
        let parent = self.writable_parent(path)?;
        parent.directory.remove(&parent.name, kind)?;
        self.forget_tree_files(path)?;
        Ok(())
    }

    pub fn validate_remove(&self, path: &VfsPathBuf, kind: RemoveKind) -> VfsResult<()> {
        if self.compiled.is_mount_point(path)? || self.compiled.has_descendant(path)? {
            return Err(VfsError::Unsupported(
                "mounted nodes cannot be removed".into(),
            ));
        }
        let parent = self.writable_parent(path)?;
        let node = parent
            .directory
            .lookup(&parent.name)?
            .ok_or_else(|| VfsError::NotFound(path.clone()))?;
        match (node, kind) {
            (Node::File(_), RemoveKind::File) => Ok(()),
            (Node::File(_), RemoveKind::Directory) => Err(VfsError::NotDirectory(path.clone())),
            (Node::Directory(_), RemoveKind::File)
            | (Node::SyntheticDirectory, RemoveKind::File) => {
                Err(VfsError::IsDirectory(path.clone()))
            }
            (Node::Directory(directory), RemoveKind::Directory) => {
                if let Some(entry) = directory.read_dir()?.next() {
                    entry?;
                    Err(VfsError::DirectoryNotEmpty(path.clone()))
                } else {
                    Ok(())
                }
            }
            (Node::SyntheticDirectory, RemoveKind::Directory) => {
                Err(VfsError::PermissionDenied(path.clone()))
            }
        }
    }

    pub fn rename(&self, source: &VfsPathBuf, target: &VfsPathBuf) -> VfsResult<()> {
        self.rename_with_options(source, target, false)
    }

    pub fn rename_with_options(
        &self,
        source: &VfsPathBuf,
        target: &VfsPathBuf,
        replace_if_exists: bool,
    ) -> VfsResult<()> {
        let source_is_mount = self.compiled.is_mount_point(source)?;
        let target_is_mount = self.compiled.is_mount_point(target)?;
        if replace_if_exists && !source_is_mount && target_is_mount {
            return self.adopt_source_node(source, target);
        }
        if source_is_mount
            || self.compiled.has_descendant(source)?
            || target_is_mount
            || self.compiled.has_descendant(target)?
        {
            return Err(VfsError::Unsupported(
                "mounted nodes cannot be renamed or replaced".into(),
            ));
        }
        let source_path = source.clone();
        let target_path = target.clone();
        let source = self.writable_parent(source)?;
        let target = self.writable_parent(target)?;
        if !Arc::ptr_eq(&source.volume, &target.volume) {
            return Err(VfsError::CrossDevice);
        }
        if replace_if_exists {
            source.volume.replace(
                source.child_relative.as_path(),
                target.child_relative.as_path(),
            )?;
        } else {
            source.volume.rename(
                source.child_relative.as_path(),
                target.child_relative.as_path(),
            )?;
        }
        self.forget_renamed_tree_files(source_path, target_path)?;
        Ok(())
    }

    fn adopt_source_node(&self, source: &VfsPathBuf, target: &VfsPathBuf) -> VfsResult<()> {
        if self.compiled.has_descendant(source)? || self.compiled.has_descendant(target)? {
            return Err(VfsError::Unsupported(
                "nodes containing mounts cannot be adopted".into(),
            ));
        }
        let source_node = self.resolve_node(source)?;
        if !matches!(&source_node.node, Node::File(_)) {
            return Err(VfsError::IsDirectory(source.clone()));
        }
        let source_file = source_node.namespace_file.ok_or(VfsError::StaleNode)?;
        let source_parent = self.writable_parent(source)?;

        let target_binding = self
            .compiled
            .resolve(target)?
            .filter(|binding| binding.at() == target)
            .ok_or_else(|| VfsError::NotFound(target.clone()))?;
        if !target_binding.permissions().can_write() {
            return Err(VfsError::PermissionDenied(target.clone()));
        }
        let target_node = self.resolve_node(target)?;
        if !matches!(target_node.node, Node::File(_)) {
            return Err(VfsError::IsDirectory(target.clone()));
        }
        let target_file = target_node.namespace_file.ok_or(VfsError::StaleNode)?;
        let adoption = source_file.prepare_adoption(&target_file)?;

        source_parent
            .volume
            .detach(source_parent.child_relative.as_path())?;
        source_file.install_adoption(adoption, &target_file);

        let mut state = self.materialized.lock();
        let resources = state.as_mut().ok_or_else(not_materialized)?;
        resources
            .tree_files
            .remove(&source.normalized_components()?);
        match target_binding.kind() {
            crate::BindingKind::File(resource) => {
                resources.files.insert(resource_key(resource), source_file);
            }
            crate::BindingKind::Storage { .. } => {
                let mounted = resources
                    .storage_mounts
                    .get_mut(&target.normalized_components()?)
                    .ok_or(VfsError::StaleNode)?;
                mounted.node = Node::File(source_file.clone());
                mounted.namespace_file = Some(source_file);
                mounted.volume = None;
            }
            crate::BindingKind::Tree(_) => {
                return Err(VfsError::IsDirectory(target.clone()));
            }
        }
        Ok(())
    }

    fn remember_tree_file(
        &self,
        path: &VfsPathBuf,
        backend: Arc<dyn FileNode>,
    ) -> VfsResult<Arc<NamespaceFile>> {
        let key = path.normalized_components()?;
        let mut state = self.materialized.lock();
        let resources = state.as_mut().ok_or_else(not_materialized)?;
        Ok(resources
            .tree_files
            .entry(key)
            .or_insert_with(|| Arc::new(NamespaceFile::new(backend)))
            .clone())
    }

    fn forget_tree_files(&self, path: &VfsPathBuf) -> VfsResult<()> {
        let prefix = path.normalized_components()?;
        let mut state = self.materialized.lock();
        let resources = state.as_mut().ok_or_else(not_materialized)?;
        resources
            .tree_files
            .retain(|path, _| !path.starts_with(&prefix));
        Ok(())
    }

    fn forget_renamed_tree_files(&self, source: VfsPathBuf, target: VfsPathBuf) -> VfsResult<()> {
        let source = source.normalized_components()?;
        let target = target.normalized_components()?;
        let mut state = self.materialized.lock();
        let resources = state.as_mut().ok_or_else(not_materialized)?;
        // File nodes supplied by storage-backed volumes are bound to the path
        // at which they were materialized. Moving their cache entries would
        // leave the destination backed by a node that still resolves the old
        // path. Existing open handles retain their own Arc, so only invalidate
        // namespace cache entries and materialize fresh nodes on the next walk.
        resources
            .tree_files
            .retain(|path, _| !path.starts_with(&source) && !path.starts_with(&target));
        Ok(())
    }

    fn writable_parent(&self, path: &VfsPathBuf) -> VfsResult<WritableParent> {
        let parent_path = path
            .parent()
            .ok_or_else(|| VfsError::Unsupported("the namespace root cannot be mutated".into()))?;
        let name = path_component(path.as_path())?;
        let resolved = self.resolve_node(&parent_path)?;
        if !resolved.permissions.can_write() || !resolved.permissions.can_execute() {
            return Err(VfsError::PermissionDenied(parent_path));
        }
        let Some(volume) = resolved.volume else {
            return Err(match resolved.node {
                Node::SyntheticDirectory => VfsError::PermissionDenied(parent_path),
                _ => VfsError::NotDirectory(parent_path),
            });
        };
        let relative = resolved
            .relative
            .expect("volume nodes carry a mount-relative path");
        let directory = match resolved.node {
            Node::Directory(directory) => directory,
            Node::SyntheticDirectory => materialize_directory(&volume, relative.as_path())?,
            Node::File(_) => return Err(VfsError::NotDirectory(parent_path)),
        };
        let child_relative = relative.as_path().join(&name);
        Ok(WritableParent {
            volume,
            directory,
            name,
            child_relative,
        })
    }

    fn resolve_node(&self, path: &VfsPathBuf) -> VfsResult<ResolvedNode> {
        let Some(binding) = self.compiled.resolve(path)? else {
            return if path == &VfsPathBuf::root() || self.compiled.has_descendant(path)? {
                Ok(ResolvedNode::synthetic())
            } else {
                Err(VfsError::NotFound(path.clone()))
            };
        };
        let permissions = binding.permissions();
        let relative_components = path
            .components()
            .skip(binding.depth())
            .map(|component| VfsComponent::from_units(crate::PathStr::as_slice(component).to_vec()))
            .collect::<VfsResult<Vec<_>>>()?;
        if !relative_components.is_empty() && !permissions.can_execute() {
            return Err(VfsError::PermissionDenied(path.clone()));
        }
        match binding.kind() {
            crate::BindingKind::File(resource) => {
                if !relative_components.is_empty() {
                    return Err(VfsError::NotDirectory(binding.at().clone()));
                }
                let file = self.materialized_file(resource)?;
                Ok(ResolvedNode {
                    node: Node::File(file.clone()),
                    namespace_file: Some(file),
                    volume: None,
                    relative: None,
                    permissions,
                })
            }
            crate::BindingKind::Tree(resource) => {
                let volume = self.materialized_volume(resource)?;
                let mut node = Node::Directory(volume.root()?);
                let mut relative = VfsPathBuf::root();
                for component in relative_components {
                    relative = relative.as_path().join(&component);
                    let Node::Directory(directory) = node else {
                        return Err(VfsError::NotDirectory(path.clone()));
                    };
                    match directory.lookup(&component)? {
                        Some(found) => node = found,
                        None if self.compiled.has_descendant(path)? => {
                            return Ok(ResolvedNode::synthetic_in(volume, relative, permissions));
                        }
                        None => return Err(VfsError::NotFound(path.clone())),
                    }
                }
                let (node, namespace_file) = match node {
                    Node::File(file) => {
                        let file = self.remember_tree_file(path, file)?;
                        (Node::File(file.clone()), Some(file))
                    }
                    node => (node, None),
                };
                Ok(ResolvedNode {
                    node,
                    namespace_file,
                    volume: Some(volume),
                    relative: Some(relative),
                    permissions,
                })
            }
            crate::BindingKind::Storage { .. } => {
                let mounted = self.materialized_storage_mount(binding.at())?;
                let mut node = mounted.node.clone();
                let mut namespace_file = mounted.namespace_file.clone();
                let mut relative = mounted.relative.clone();
                for component in relative_components {
                    relative = relative.as_path().join(&component);
                    let Node::Directory(directory) = node else {
                        return Err(VfsError::NotDirectory(path.clone()));
                    };
                    match directory.lookup(&component)? {
                        Some(found) => node = found,
                        None if self.compiled.has_descendant(path)? => {
                            return Ok(ResolvedNode::synthetic());
                        }
                        None => return Err(VfsError::NotFound(path.clone())),
                    }
                    namespace_file = None;
                }
                if let Node::File(file) = node {
                    let file = namespace_file.unwrap_or(self.remember_tree_file(path, file)?);
                    node = Node::File(file.clone());
                    namespace_file = Some(file);
                }
                Ok(ResolvedNode {
                    node,
                    namespace_file,
                    volume: mounted.volume.clone(),
                    relative: mounted.volume.as_ref().map(|_| relative),
                    permissions,
                })
            }
        }
    }

    fn materialized_volume(
        &self,
        resource: &Arc<Resource<dyn Volume>>,
    ) -> VfsResult<Arc<dyn Volume>> {
        self.materialized
            .lock()
            .as_ref()
            .ok_or_else(not_materialized)?
            .volumes
            .get(&resource_key(resource))
            .cloned()
            .ok_or(VfsError::StaleNode)
    }

    fn materialized_file(
        &self,
        resource: &Arc<Resource<dyn FileNode>>,
    ) -> VfsResult<Arc<NamespaceFile>> {
        self.materialized
            .lock()
            .as_ref()
            .ok_or_else(not_materialized)?
            .files
            .get(&resource_key(resource))
            .cloned()
            .ok_or(VfsError::StaleNode)
    }

    fn materialized_storage_mount(&self, at: &VfsPathBuf) -> VfsResult<MaterializedStorageMount> {
        self.materialized
            .lock()
            .as_ref()
            .ok_or_else(not_materialized)?
            .storage_mounts
            .get(&at.normalized_components()?)
            .cloned()
            .ok_or(VfsError::StaleNode)
    }
}

fn resource_key<T: ?Sized>(resource: &Arc<Resource<T>>) -> usize {
    Arc::as_ptr(resource) as *const () as usize
}

fn not_materialized() -> VfsError {
    VfsError::Unsupported("filesystem has not been materialized by a host".into())
}

fn path_component(path: &VfsPath) -> VfsResult<VfsComponent> {
    path.components()
        .next_back()
        .map(|name| VfsComponent::from_units(crate::PathStr::as_slice(name).to_vec()))
        .transpose()?
        .ok_or_else(|| VfsError::Unsupported("the namespace root has no name".into()))
}

struct WritableParent {
    volume: Arc<dyn Volume>,
    directory: Arc<dyn DirectoryNode>,
    name: VfsComponent,
    child_relative: VfsPathBuf,
}

struct ResolvedNode {
    node: Node,
    namespace_file: Option<Arc<NamespaceFile>>,
    volume: Option<Arc<dyn Volume>>,
    relative: Option<VfsPathBuf>,
    permissions: Permissions,
}

impl ResolvedNode {
    fn synthetic() -> Self {
        Self {
            node: Node::SyntheticDirectory,
            namespace_file: None,
            volume: None,
            relative: None,
            permissions: Permissions::ALL,
        }
    }

    fn synthetic_in(
        volume: Arc<dyn Volume>,
        relative: VfsPathBuf,
        permissions: Permissions,
    ) -> Self {
        Self {
            node: Node::SyntheticDirectory,
            namespace_file: None,
            volume: Some(volume),
            relative: Some(relative),
            permissions,
        }
    }
}

fn materialize_directory(
    volume: &Arc<dyn Volume>,
    relative: &VfsPath,
) -> VfsResult<Arc<dyn DirectoryNode>> {
    let mut directory = volume.root()?;
    for component in relative.components() {
        let component = VfsComponent::from_units(crate::PathStr::as_slice(component).to_vec())?;
        directory = match directory.lookup(&component)? {
            Some(Node::Directory(directory)) => directory,
            Some(Node::File(_)) => return Err(VfsError::NotDirectory(relative.to_owned())),
            Some(Node::SyntheticDirectory) => return Err(VfsError::StaleNode),
            None => directory.create_dir(&component)?,
        };
    }
    Ok(directory)
}

impl Drop for PreparedFilesystem {
    fn drop(&mut self) {
        if let Some(mut resources) = self.materialized.get_mut().take()
            && resources.finalize(FinalizationReason::Cancelled).is_err()
        {
            super::retry::FinalizationRetry::retain(Arc::new(FinalizingResources(Mutex::new(
                resources,
            ))));
        }
    }
}

struct MaterializedResources {
    volumes: HashMap<usize, Arc<dyn Volume>>,
    files: HashMap<usize, Arc<NamespaceFile>>,
    storage_mounts: HashMap<Vec<Vec<VfsPathUnit>>, MaterializedStorageMount>,
    tree_files: HashMap<Vec<Vec<VfsPathUnit>>, Arc<NamespaceFile>>,
    leases: Vec<Arc<dyn ResourceLease>>,
    finalized: bool,
}

#[derive(Clone)]
struct MaterializedStorageMount {
    node: Node,
    namespace_file: Option<Arc<NamespaceFile>>,
    volume: Option<Arc<dyn Volume>>,
    relative: VfsPathBuf,
}

impl MaterializedStorageMount {
    fn new(mounted: Node, source: Arc<dyn Volume>, format: Option<&str>) -> VfsResult<Self> {
        let relative = match format {
            Some(format) => VfsPathBuf::new(format!("/{format}"))?,
            None => VfsPathBuf::root(),
        };
        Ok(match mounted {
            Node::File(file) => {
                let file = Arc::new(NamespaceFile::new(file));
                Self {
                    node: Node::File(file.clone()),
                    namespace_file: Some(file),
                    volume: None,
                    relative,
                }
            }
            node @ Node::Directory(_) => Self {
                node,
                namespace_file: None,
                volume: Some(source),
                relative,
            },
            Node::SyntheticDirectory => Self {
                node: Node::SyntheticDirectory,
                namespace_file: None,
                volume: None,
                relative,
            },
        })
    }
}

impl MaterializedResources {
    fn finalize(&mut self, reason: FinalizationReason) -> VfsResult<()> {
        if self.finalized {
            return Ok(());
        }
        if reason != FinalizationReason::LaunchFailed {
            for file in self.files.values().chain(self.tree_files.values()) {
                file.finalize_adoption()?;
            }
        }
        while let Some(lease) = self.leases.last() {
            lease.finalize(reason)?;
            self.leases.pop();
        }
        self.finalized = true;
        Ok(())
    }
}

struct FinalizingResources(Mutex<MaterializedResources>);

impl ResourceLease for FinalizingResources {
    fn finalize(&self, reason: FinalizationReason) -> VfsResult<()> {
        self.0.lock().finalize(reason)
    }
}
