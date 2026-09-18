use crate::{
    CreateOptions, DirectoryEntry, DirectoryIterator, DirectoryNode, FileKind, FileNode,
    Materialized, Metadata, MetadataChanges, Node, NoopResourceLease, OpenFile, OpenOptions,
    RemoveKind, ResourceFactory, VfsComponent, VfsError, VfsPath, VfsPathBuf, VfsPathUnit,
    VfsResult, Volume,
};
use parking_lot::{Mutex, RwLock};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

pub struct MemoryFileFactory {
    storage: Arc<polymer_consolestorage::MemoryStorage>,
    writable: bool,
}

impl MemoryFileFactory {
    pub fn immutable(bytes: Vec<u8>) -> Self {
        Self {
            storage: Arc::new(polymer_consolestorage::MemoryStorage::file(bytes, false)),
            writable: false,
        }
    }

    pub fn writable(bytes: Vec<u8>) -> Self {
        Self {
            storage: Arc::new(polymer_consolestorage::MemoryStorage::file(bytes, true)),
            writable: true,
        }
    }
}

impl ResourceFactory<dyn FileNode> for MemoryFileFactory {
    fn materialize(&self) -> VfsResult<Materialized<dyn FileNode>> {
        let writable = self.writable;
        Ok(super::storage::materialize_file(
            self.storage.clone(),
            String::new(),
            Arc::new(move |_| {
                if writable {
                    crate::Permissions::ALL
                } else {
                    crate::Permissions::READ
                }
            }),
            Arc::new(NoopResourceLease),
        ))
    }
}

struct MemoryOpen {
    bytes: Arc<RwLock<Vec<u8>>>,
    writable: bool,
}

impl OpenFile for MemoryOpen {
    fn metadata(&self) -> VfsResult<Metadata> {
        Ok(Metadata {
            kind: FileKind::File,
            len: self.bytes.read().len() as u64,
            readonly: !self.writable,
        })
    }

    fn read(&self, offset: u64, buffer: &mut [u8]) -> VfsResult<usize> {
        let offset = offset as usize;
        let bytes = self.bytes.read();
        if offset >= bytes.len() {
            return Ok(0);
        }
        let count = buffer.len().min(bytes.len() - offset);
        buffer[..count].copy_from_slice(&bytes[offset..offset + count]);
        Ok(count)
    }

    fn write(&self, offset: u64, buffer: &[u8]) -> VfsResult<usize> {
        VfsError::require_writable(self.writable)?;
        let offset = offset as usize;
        let end = offset
            .checked_add(buffer.len())
            .ok_or_else(|| VfsError::Unsupported("file offset overflow".into()))?;
        let mut bytes = self.bytes.write();
        if bytes.len() < end {
            bytes.resize(end, 0);
        }
        bytes[offset..end].copy_from_slice(buffer);
        Ok(buffer.len())
    }

    fn truncate(&self, len: u64) -> VfsResult<()> {
        VfsError::require_writable(self.writable)?;
        self.bytes.write().resize(len as usize, 0);
        Ok(())
    }
    fn flush(&self) -> VfsResult<()> {
        Ok(())
    }
    fn sync(&self) -> VfsResult<()> {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScratchBacking {
    Memory,
    Temporary,
}

pub struct ScratchVolumeFactory {
    backing: ScratchBacking,
    temporary: super::host::TempDirectoryFactory,
}

impl ScratchVolumeFactory {
    pub const fn new(backing: ScratchBacking) -> Self {
        Self {
            backing,
            temporary: super::host::TempDirectoryFactory::new(),
        }
    }

    pub fn temporary_in(root: std::path::PathBuf) -> Self {
        Self {
            backing: ScratchBacking::Temporary,
            temporary: super::host::TempDirectoryFactory::in_root(root),
        }
    }
}

impl ResourceFactory<dyn Volume> for ScratchVolumeFactory {
    fn materialize(&self) -> VfsResult<Materialized<dyn Volume>> {
        match self.backing {
            ScratchBacking::Memory => Ok(Materialized {
                backend: Arc::new(MemoryVolume::new()),
                lease: Arc::new(NoopResourceLease),
            }),
            ScratchBacking::Temporary => self.temporary.materialize(),
        }
    }
}

struct MemoryVolume {
    state: Arc<Mutex<MemoryState>>,
}

struct MemoryState {
    next_node: u64,
    nodes: HashMap<u64, MemoryNode>,
}

struct MemoryNode {
    kind: MemoryKind,
    readonly: bool,
}

enum MemoryKind {
    Directory(BTreeMap<Vec<VfsPathUnit>, (VfsComponent, u64)>),
    File(Arc<RwLock<Vec<u8>>>),
}

#[derive(Clone)]
struct MemoryDirectory {
    state: Arc<Mutex<MemoryState>>,
    node: u64,
}

#[derive(Clone)]
struct MemoryTreeFile {
    state: Arc<Mutex<MemoryState>>,
    node: u64,
}

impl MemoryVolume {
    const ROOT: u64 = 1;

    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(MemoryState {
                next_node: Self::ROOT + 1,
                nodes: HashMap::from([(
                    Self::ROOT,
                    MemoryNode {
                        kind: MemoryKind::Directory(BTreeMap::new()),
                        readonly: false,
                    },
                )]),
            })),
        }
    }

    fn component(path: &VfsPath) -> VfsResult<VfsComponent> {
        path.components()
            .next_back()
            .map(|name| VfsComponent::from_units(crate::PathStr::as_slice(name).to_vec()))
            .transpose()?
            .ok_or_else(|| VfsError::Unsupported("the volume root has no name".into()))
    }

    fn resolve_locked(state: &MemoryState, path: &VfsPath) -> VfsResult<u64> {
        let mut node = Self::ROOT;
        for name in path.components() {
            let name: VfsComponent =
                VfsComponent::from_units(crate::PathStr::as_slice(name).to_vec())?;
            let current = state.nodes.get(&node).expect("resolved node exists");
            let MemoryKind::Directory(children) = &current.kind else {
                return Err(VfsError::NotDirectory(path.to_owned()));
            };
            node = children
                .get(&name.normalized()?)
                .map(|(_, child)| *child)
                .ok_or_else(|| VfsError::NotFound(path.to_owned()))?;
        }
        Ok(node)
    }
}

impl MemoryState {
    fn allocate(&mut self, kind: MemoryKind) -> u64 {
        let node = self.next_node;
        self.next_node = self
            .next_node
            .checked_add(1)
            .expect("memory node space exhausted");
        self.nodes.insert(
            node,
            MemoryNode {
                kind,
                readonly: false,
            },
        );
        node
    }

    fn node(&self, state: &Arc<Mutex<Self>>, node: u64) -> VfsResult<Node> {
        match &self.nodes.get(&node).ok_or(VfsError::StaleNode)?.kind {
            MemoryKind::File(_) => Ok(Node::File(Arc::new(MemoryTreeFile {
                state: state.clone(),
                node,
            }))),
            MemoryKind::Directory(_) => Ok(Node::Directory(Arc::new(MemoryDirectory {
                state: state.clone(),
                node,
            }))),
        }
    }
}

impl Volume for MemoryVolume {
    fn root(&self) -> VfsResult<Arc<dyn DirectoryNode>> {
        Ok(Arc::new(MemoryDirectory {
            state: self.state.clone(),
            node: Self::ROOT,
        }))
    }

    fn rename(&self, source: &VfsPath, target: &VfsPath) -> VfsResult<()> {
        self.rename_inner(source, target, false)
    }

    fn replace(&self, source: &VfsPath, target: &VfsPath) -> VfsResult<()> {
        self.rename_inner(source, target, true)
    }

    fn detach(&self, source: &VfsPath) -> VfsResult<()> {
        let parent_path = source
            .parent()
            .ok_or_else(|| VfsError::Unsupported("the volume root cannot be detached".into()))?;
        let name = Self::component(source)?;
        let key = name.normalized()?;
        let mut state = self.state.lock();
        let parent = Self::resolve_locked(&state, parent_path.as_path())?;
        let child = {
            let parent = state.nodes.get(&parent).expect("source parent exists");
            let MemoryKind::Directory(children) = &parent.kind else {
                return Err(VfsError::NotDirectory(parent_path));
            };
            children
                .get(&key)
                .map(|(_, child)| *child)
                .ok_or_else(|| VfsError::NotFound(source.to_owned()))?
        };
        if !matches!(
            state.nodes.get(&child).expect("source child exists").kind,
            MemoryKind::File(_)
        ) {
            return Err(VfsError::IsDirectory(source.to_owned()));
        }
        let MemoryKind::Directory(children) = &mut state
            .nodes
            .get_mut(&parent)
            .expect("source parent exists")
            .kind
        else {
            unreachable!()
        };
        children.remove(&key);
        Ok(())
    }
}

impl MemoryVolume {
    fn rename_inner(
        &self,
        source: &VfsPath,
        target: &VfsPath,
        replace_if_exists: bool,
    ) -> VfsResult<()> {
        let source_parent_path = source
            .parent()
            .ok_or_else(|| VfsError::Unsupported("the volume root cannot be renamed".into()))?;
        let target_parent_path = target
            .parent()
            .ok_or_else(|| VfsError::Unsupported("the volume root cannot be replaced".into()))?;
        let source_name = Self::component(source)?;
        let target_name = Self::component(target)?;
        let source_key = source_name.normalized()?;
        let target_key = target_name.normalized()?;
        let mut state = self.state.lock();
        let source_parent = Self::resolve_locked(&state, source_parent_path.as_path())?;
        let target_parent = Self::resolve_locked(&state, target_parent_path.as_path())?;
        let child = {
            let parent = state
                .nodes
                .get(&source_parent)
                .expect("source parent exists");
            let MemoryKind::Directory(children) = &parent.kind else {
                return Err(VfsError::NotDirectory(source_parent_path));
            };
            children
                .get(&source_key)
                .map(|(_, child)| *child)
                .ok_or_else(|| VfsError::NotFound(source.to_owned()))?
        };
        let replaced = {
            let parent = state
                .nodes
                .get(&target_parent)
                .expect("target parent exists");
            let MemoryKind::Directory(children) = &parent.kind else {
                return Err(VfsError::NotDirectory(target_parent_path));
            };
            children.get(&target_key).map(|(_, child)| *child)
        };
        if replaced == Some(child) {
            let MemoryKind::Directory(children) = &mut state
                .nodes
                .get_mut(&source_parent)
                .expect("source parent exists")
                .kind
            else {
                unreachable!()
            };
            children.insert(target_key, (target_name, child));
            return Ok(());
        }
        if let Some(replaced) = replaced {
            if !replace_if_exists {
                return Err(VfsError::AlreadyExists(target.to_owned()));
            }
            match (
                &state.nodes.get(&child).expect("source child exists").kind,
                &state
                    .nodes
                    .get(&replaced)
                    .expect("target child exists")
                    .kind,
            ) {
                (MemoryKind::File(_), MemoryKind::Directory(_)) => {
                    return Err(VfsError::IsDirectory(target.to_owned()));
                }
                (MemoryKind::Directory(_), MemoryKind::File(_)) => {
                    return Err(VfsError::NotDirectory(target.to_owned()));
                }
                (_, MemoryKind::Directory(children)) if !children.is_empty() => {
                    return Err(VfsError::DirectoryNotEmpty(target.to_owned()));
                }
                _ => {}
            }
        }
        let MemoryKind::Directory(children) = &mut state
            .nodes
            .get_mut(&source_parent)
            .expect("source parent exists")
            .kind
        else {
            unreachable!()
        };
        children.remove(&source_key);
        if let Some(replaced) = replaced {
            let MemoryKind::Directory(children) = &mut state
                .nodes
                .get_mut(&target_parent)
                .expect("target parent exists")
                .kind
            else {
                unreachable!()
            };
            children.remove(&target_key);
            state.nodes.remove(&replaced);
        }
        let MemoryKind::Directory(children) = &mut state
            .nodes
            .get_mut(&target_parent)
            .expect("target parent exists")
            .kind
        else {
            unreachable!()
        };
        children.insert(target_key, (target_name, child));
        Ok(())
    }
}

impl DirectoryNode for MemoryDirectory {
    fn metadata(&self) -> VfsResult<Metadata> {
        let state = self.state.lock();
        let node = state.nodes.get(&self.node).ok_or(VfsError::StaleNode)?;
        Ok(Metadata {
            kind: FileKind::Directory,
            len: 0,
            readonly: node.readonly,
        })
    }

    fn set_metadata(&self, changes: MetadataChanges) -> VfsResult<()> {
        if changes.len.is_some() {
            return Err(VfsError::IsDirectory(VfsPathBuf::root()));
        }
        if let Some(readonly) = changes.readonly {
            self.state
                .lock()
                .nodes
                .get_mut(&self.node)
                .ok_or(VfsError::StaleNode)?
                .readonly = readonly;
        }
        Ok(())
    }

    fn lookup(&self, name: &VfsComponent) -> VfsResult<Option<Node>> {
        let state = self.state.lock();
        let current = state.nodes.get(&self.node).ok_or(VfsError::StaleNode)?;
        let MemoryKind::Directory(children) = &current.kind else {
            return Err(VfsError::NotDirectory(VfsPathBuf::root()));
        };
        children
            .get(&name.normalized()?)
            .map(|(_, child)| state.node(&self.state, *child))
            .transpose()
    }

    fn read_dir(&self) -> VfsResult<Box<DirectoryIterator>> {
        let state = self.state.lock();
        let current = state.nodes.get(&self.node).ok_or(VfsError::StaleNode)?;
        let MemoryKind::Directory(children) = &current.kind else {
            return Err(VfsError::NotDirectory(VfsPathBuf::root()));
        };
        let entries = children
            .values()
            .map(|(name, child)| {
                let child = state.nodes.get(child).ok_or(VfsError::StaleNode)?;
                let metadata = Metadata {
                    kind: match &child.kind {
                        MemoryKind::Directory(_) => FileKind::Directory,
                        MemoryKind::File(_) => FileKind::File,
                    },
                    len: match &child.kind {
                        MemoryKind::Directory(_) => 0,
                        MemoryKind::File(bytes) => bytes.read().len() as u64,
                    },
                    readonly: child.readonly,
                };
                Ok(DirectoryEntry {
                    name: name.clone(),
                    metadata,
                })
            })
            .collect::<Vec<_>>();
        Ok(Box::new(entries.into_iter()))
    }

    fn create_file(
        &self,
        name: &VfsComponent,
        options: CreateOptions,
    ) -> VfsResult<Arc<dyn FileNode>> {
        let key = name.normalized()?;
        let mut state = self.state.lock();
        let existing = {
            let parent = state.nodes.get(&self.node).ok_or(VfsError::StaleNode)?;
            let MemoryKind::Directory(children) = &parent.kind else {
                return Err(VfsError::NotDirectory(VfsPathBuf::root()));
            };
            children.get(&key).map(|(_, child)| *child)
        };
        if let Some(node) = existing {
            if options.exclusive {
                return Err(VfsError::AlreadyExists(VfsPathBuf::root()));
            }
            if matches!(
                state.nodes.get(&node).unwrap().kind,
                MemoryKind::Directory(_)
            ) {
                return Err(VfsError::IsDirectory(VfsPathBuf::root()));
            }
            return Ok(Arc::new(MemoryTreeFile {
                state: self.state.clone(),
                node,
            }));
        }
        let node = state.allocate(MemoryKind::File(Arc::new(RwLock::new(Vec::new()))));
        let MemoryKind::Directory(children) = &mut state
            .nodes
            .get_mut(&self.node)
            .expect("parent was validated")
            .kind
        else {
            unreachable!()
        };
        children.insert(key, (name.clone(), node));
        Ok(Arc::new(MemoryTreeFile {
            state: self.state.clone(),
            node,
        }))
    }

    fn create_dir(&self, name: &VfsComponent) -> VfsResult<Arc<dyn DirectoryNode>> {
        let key = name.normalized()?;
        let mut state = self.state.lock();
        {
            let parent = state.nodes.get(&self.node).ok_or(VfsError::StaleNode)?;
            let MemoryKind::Directory(children) = &parent.kind else {
                return Err(VfsError::NotDirectory(VfsPathBuf::root()));
            };
            if children.contains_key(&key) {
                return Err(VfsError::AlreadyExists(VfsPathBuf::root()));
            }
        }
        let node = state.allocate(MemoryKind::Directory(BTreeMap::new()));
        let MemoryKind::Directory(children) = &mut state
            .nodes
            .get_mut(&self.node)
            .expect("parent was validated")
            .kind
        else {
            unreachable!()
        };
        children.insert(key, (name.clone(), node));
        Ok(Arc::new(MemoryDirectory {
            state: self.state.clone(),
            node,
        }))
    }

    fn remove(&self, name: &VfsComponent, kind: RemoveKind) -> VfsResult<()> {
        let key = name.normalized()?;
        let mut state = self.state.lock();
        let child = {
            let parent = state.nodes.get(&self.node).ok_or(VfsError::StaleNode)?;
            let MemoryKind::Directory(children) = &parent.kind else {
                return Err(VfsError::NotDirectory(VfsPathBuf::root()));
            };
            children
                .get(&key)
                .map(|(_, child)| *child)
                .ok_or_else(|| VfsError::NotFound(VfsPathBuf::root()))?
        };
        match (&state.nodes.get(&child).expect("child exists").kind, kind) {
            (MemoryKind::Directory(children), RemoveKind::Directory) if !children.is_empty() => {
                return Err(VfsError::DirectoryNotEmpty(VfsPathBuf::root()));
            }
            (MemoryKind::Directory(_), RemoveKind::File) => {
                return Err(VfsError::IsDirectory(VfsPathBuf::root()));
            }
            (MemoryKind::File(_), RemoveKind::Directory) => {
                return Err(VfsError::NotDirectory(VfsPathBuf::root()));
            }
            _ => {}
        }
        let MemoryKind::Directory(children) =
            &mut state.nodes.get_mut(&self.node).expect("parent exists").kind
        else {
            unreachable!()
        };
        children.remove(&key);
        state.nodes.remove(&child);
        Ok(())
    }
}

impl FileNode for MemoryTreeFile {
    fn metadata(&self) -> VfsResult<Metadata> {
        let state = self.state.lock();
        let node = state.nodes.get(&self.node).ok_or(VfsError::StaleNode)?;
        let MemoryKind::File(bytes) = &node.kind else {
            return Err(VfsError::IsDirectory(VfsPathBuf::root()));
        };
        Ok(Metadata {
            kind: FileKind::File,
            len: bytes.read().len() as u64,
            readonly: node.readonly,
        })
    }

    fn set_metadata(&self, changes: MetadataChanges) -> VfsResult<()> {
        let mut state = self.state.lock();
        let node = state.nodes.get_mut(&self.node).ok_or(VfsError::StaleNode)?;
        if let Some(readonly) = changes.readonly {
            node.readonly = readonly;
        }
        if let Some(len) = changes.len {
            VfsError::require_writable(!node.readonly)?;
            let MemoryKind::File(bytes) = &node.kind else {
                return Err(VfsError::IsDirectory(VfsPathBuf::root()));
            };
            bytes.write().resize(len as usize, 0);
        }
        Ok(())
    }

    fn open(&self, options: OpenOptions) -> VfsResult<Box<dyn OpenFile>> {
        let state = self.state.lock();
        let node = state.nodes.get(&self.node).ok_or(VfsError::StaleNode)?;
        let MemoryKind::File(bytes) = &node.kind else {
            return Err(VfsError::IsDirectory(VfsPathBuf::root()));
        };
        if options.write || options.truncate {
            VfsError::require_writable(!node.readonly)?;
        }
        if options.truncate {
            bytes.write().clear();
        }
        Ok(Box::new(MemoryOpen {
            bytes: bytes.clone(),
            writable: !node.readonly,
        }))
    }
}
