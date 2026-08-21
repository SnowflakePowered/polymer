use crate::{
    CompiledFilesystem, CreateOptions, DirectoryEntry, DirectoryNode, FileNode, Metadata,
    MetadataChanges, Node, OpenFile, OpenOptions, Permissions, RemoveKind, VfsComponent, VfsError,
    VfsPath, VfsPathBuf, VfsPathUnit, VfsResult, Volume,
};
use parking_lot::Mutex;
use std::{collections::HashMap, fmt, sync::Arc};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResourceOrigin {
    Memory,
    Scratch,
    Cache(String),
    Package(String),
    Asar(String),
    HostFile,
    Other(String),
}

pub struct Resource<T: ?Sized> {
    factory: Arc<dyn ResourceFactory<T>>,
    authority: Permissions,
    origin: ResourceOrigin,
}

impl<T: ?Sized> Resource<T> {
    pub fn new(
        factory: Arc<dyn ResourceFactory<T>>,
        authority: Permissions,
        origin: ResourceOrigin,
    ) -> Self {
        Self {
            factory,
            authority,
            origin,
        }
    }

    pub const fn authority(&self) -> Permissions {
        self.authority
    }

    pub fn origin(&self) -> &ResourceOrigin {
        &self.origin
    }

    pub fn materialize(&self) -> VfsResult<Materialized<T>> {
        self.factory.materialize()
    }
}

impl<T: ?Sized> fmt::Debug for Resource<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Resource")
            .field("authority", &self.authority)
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

pub trait ResourceFactory<T: ?Sized>: Send + Sync {
    fn materialize(&self) -> VfsResult<Materialized<T>>;
}

pub struct Materialized<T: ?Sized> {
    pub backend: Arc<T>,
    pub lease: Arc<dyn ResourceLease>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FinalizationReason {
    LaunchFailed,
    ProcessExited,
    Cancelled,
}

pub trait ResourceLease: Send + Sync {
    fn finalize(&self, reason: FinalizationReason);
}

#[derive(Default)]
pub struct NoopResourceLease;

impl ResourceLease for NoopResourceLease {
    fn finalize(&self, _reason: FinalizationReason) {}
}

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
        let mut leases = Vec::new();
        for binding in self.compiled.bindings() {
            match binding.kind() {
                crate::BindingKind::Tree(resource) => {
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
                        entry.insert(value.backend);
                    }
                }
            }
        }
        *state = Some(MaterializedResources {
            volumes,
            files,
            leases,
            finalized: false,
        });
        Ok(())
    }

    pub fn finalize(&self, reason: FinalizationReason) {
        if let Some(resources) = self.materialized.lock().as_mut() {
            resources.finalize(reason);
        }
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
            entries.insert(name.normalized()?, DirectoryEntry { name, node });
        }
        Ok(entries.into_values().collect())
    }

    pub fn create_file(&self, path: &VfsPathBuf, options: CreateOptions) -> VfsResult<()> {
        if self.compiled.is_mount_point(path)? {
            return Err(VfsError::AlreadyExists(path.clone()));
        }
        let parent = self.writable_parent(path)?;
        parent.directory.create_file(&parent.name, options)?;
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
        parent.directory.remove(&parent.name, kind)
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
        if self.compiled.is_mount_point(source)?
            || self.compiled.has_descendant(source)?
            || self.compiled.is_mount_point(target)?
            || self.compiled.has_descendant(target)?
        {
            return Err(VfsError::Unsupported(
                "mounted nodes cannot be renamed or replaced".into(),
            ));
        }
        let source = self.writable_parent(source)?;
        let target = self.writable_parent(target)?;
        if !Arc::ptr_eq(&source.volume, &target.volume) {
            return Err(VfsError::CrossDevice);
        }
        if replace_if_exists {
            source.volume.replace(
                source.child_relative.as_path(),
                target.child_relative.as_path(),
            )
        } else {
            source.volume.rename(
                source.child_relative.as_path(),
                target.child_relative.as_path(),
            )
        }
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
        let Node::Directory(directory) = resolved.node else {
            return Err(VfsError::NotDirectory(parent_path));
        };
        let Some(volume) = resolved.volume else {
            return Err(VfsError::PermissionDenied(parent_path));
        };
        let relative = resolved
            .relative
            .expect("volume nodes carry a mount-relative path");
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
                    node: Node::File(file),
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
                            return Ok(ResolvedNode::synthetic());
                        }
                        None => return Err(VfsError::NotFound(path.clone())),
                    }
                }
                Ok(ResolvedNode {
                    node,
                    volume: Some(volume),
                    relative: Some(relative),
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
    ) -> VfsResult<Arc<dyn FileNode>> {
        self.materialized
            .lock()
            .as_ref()
            .ok_or_else(not_materialized)?
            .files
            .get(&resource_key(resource))
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
    volume: Option<Arc<dyn Volume>>,
    relative: Option<VfsPathBuf>,
    permissions: Permissions,
}

impl ResolvedNode {
    fn synthetic() -> Self {
        Self {
            node: Node::SyntheticDirectory,
            volume: None,
            relative: None,
            permissions: Permissions::ALL,
        }
    }
}

impl Drop for PreparedFilesystem {
    fn drop(&mut self) {
        if let Some(resources) = self.materialized.get_mut().as_mut() {
            resources.finalize(FinalizationReason::Cancelled);
        }
    }
}

struct MaterializedResources {
    volumes: HashMap<usize, Arc<dyn Volume>>,
    files: HashMap<usize, Arc<dyn FileNode>>,
    leases: Vec<Arc<dyn ResourceLease>>,
    finalized: bool,
}

impl MaterializedResources {
    fn finalize(&mut self, reason: FinalizationReason) {
        if self.finalized {
            return;
        }
        for lease in self.leases.iter().rev() {
            lease.finalize(reason);
        }
        self.finalized = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AsarVolumeFactory, FilesystemHost, FilesystemPlan, MemoryFileFactory, MockFilesystemHost,
        ResourceOrigin, ScratchBacking, ScratchVolumeFactory,
    };
    use std::io::Write;

    fn file(bytes: &[u8]) -> Arc<Resource<dyn FileNode>> {
        Arc::new(Resource::new(
            Arc::new(MemoryFileFactory::immutable(bytes.to_vec())),
            Permissions::READ,
            ResourceOrigin::Memory,
        ))
    }

    fn scratch() -> Arc<Resource<dyn Volume>> {
        Arc::new(Resource::new(
            Arc::new(ScratchVolumeFactory::new(ScratchBacking::Memory)),
            Permissions::ALL,
            ResourceOrigin::Scratch,
        ))
    }

    #[test]
    fn directory_views_merge_recursive_submounts() {
        let mut plan = FilesystemPlan::default();
        plan.mount_tree("/Parent".parse().unwrap(), scratch(), Permissions::ALL);
        plan.mount_file(
            "/Parent/child.txt".parse().unwrap(),
            file(b"override"),
            Permissions::READ,
        );
        plan.mount_file(
            "/Parent/Nested/value.bin".parse().unwrap(),
            file(b"nested"),
            Permissions::READ,
        );
        let prepared = Arc::new(plan.seal().unwrap());
        let host = MockFilesystemHost;
        let mounted = host.mount(host.allocate_endpoint(), prepared).unwrap();
        let root = mounted
            .filesystem()
            .read_dir(&"/Parent".parse().unwrap())
            .unwrap();
        assert_eq!(
            root.iter()
                .map(|entry| entry.name.to_string())
                .collect::<Vec<_>>(),
            ["child.txt", "Nested"]
        );
        let nested = mounted
            .filesystem()
            .read_dir(&"/Parent/Nested".parse().unwrap())
            .unwrap();
        assert_eq!(nested[0].name.to_string(), "value.bin");
    }

    #[test]
    fn mutations_cannot_cross_or_replace_mounts() {
        let mut plan = FilesystemPlan::default();
        plan.mount_tree("/A".parse().unwrap(), scratch(), Permissions::ALL);
        plan.mount_tree("/B".parse().unwrap(), scratch(), Permissions::ALL);
        plan.mount_file("/A/mounted".parse().unwrap(), file(&[]), Permissions::READ);
        let prepared = Arc::new(plan.seal().unwrap());
        let host = MockFilesystemHost;
        let mounted = host.mount(host.allocate_endpoint(), prepared).unwrap();
        mounted
            .filesystem()
            .create_file(
                &"/A/file".parse().unwrap(),
                CreateOptions { exclusive: true },
            )
            .unwrap();
        assert!(matches!(
            mounted
                .filesystem()
                .rename(&"/A/file".parse().unwrap(), &"/B/file".parse().unwrap()),
            Err(VfsError::CrossDevice)
        ));
        assert!(
            mounted
                .filesystem()
                .remove(&"/A/mounted".parse().unwrap(), RemoveKind::File)
                .is_err()
        );
    }

    #[test]
    fn scratch_files_can_be_atomically_replaced() {
        let mut plan = FilesystemPlan::default();
        plan.mount_tree("/Scratch".parse().unwrap(), scratch(), Permissions::ALL);
        let prepared = Arc::new(plan.seal().unwrap());
        let host = MockFilesystemHost;
        let mounted = host.mount(host.allocate_endpoint(), prepared).unwrap();
        let filesystem = mounted.filesystem();
        let source = "/Scratch/settings.ini.new".parse().unwrap();
        let target = "/Scratch/settings.ini".parse().unwrap();

        filesystem
            .create_file(&source, CreateOptions { exclusive: true })
            .unwrap();
        filesystem
            .open(
                &source,
                OpenOptions {
                    read: false,
                    write: true,
                    truncate: false,
                },
            )
            .unwrap()
            .write_at(0, b"new")
            .unwrap();
        filesystem
            .create_file(&target, CreateOptions { exclusive: true })
            .unwrap();
        filesystem
            .open(
                &target,
                OpenOptions {
                    read: false,
                    write: true,
                    truncate: false,
                },
            )
            .unwrap()
            .write_at(0, b"old")
            .unwrap();

        assert!(matches!(
            filesystem.rename(&source, &target),
            Err(VfsError::AlreadyExists(_))
        ));
        filesystem
            .rename_with_options(&source, &target, true)
            .unwrap();

        let mut contents = [0; 3];
        filesystem
            .open(
                &target,
                OpenOptions {
                    read: true,
                    write: false,
                    truncate: false,
                },
            )
            .unwrap()
            .read_at(0, &mut contents)
            .unwrap();
        assert_eq!(&contents, b"new");
        assert!(matches!(
            filesystem.metadata(&source),
            Err(VfsError::NotFound(_))
        ));
    }

    #[test]
    fn individual_files_override_entries_inside_asar_trees() {
        let mut archive = tempfile::NamedTempFile::new().unwrap();
        let mut writer = asar::AsarWriter::new();
        writer
            .write_file("Config/settings.ini", b"archive", false)
            .unwrap();
        writer.finalize(&mut archive).unwrap();
        archive.flush().unwrap();

        let tree = Arc::new(Resource::new(
            Arc::new(AsarVolumeFactory::new(archive.path().to_owned())),
            "rx".parse().unwrap(),
            ResourceOrigin::Asar(archive.path().display().to_string()),
        ));
        let mut plan = FilesystemPlan::default();
        plan.mount_tree("/Emulator".parse().unwrap(), tree, "rx".parse().unwrap());
        plan.mount_file(
            "/Emulator/Config/settings.ini".parse().unwrap(),
            file(b"memory"),
            Permissions::READ,
        );
        let prepared = Arc::new(plan.seal().unwrap());
        let host = MockFilesystemHost;
        let mounted = host.mount(host.allocate_endpoint(), prepared).unwrap();
        let file = mounted
            .filesystem()
            .open(
                &"/Emulator/Config/settings.ini".parse().unwrap(),
                OpenOptions {
                    read: true,
                    write: false,
                    truncate: false,
                },
            )
            .unwrap();
        let mut bytes = [0; 6];
        file.read_at(0, &mut bytes).unwrap();
        assert_eq!(&bytes, b"memory");
    }
}
