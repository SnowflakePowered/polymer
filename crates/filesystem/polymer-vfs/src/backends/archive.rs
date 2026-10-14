use crate::{
    CreateOptions, DirectoryEntry, DirectoryIterator, DirectoryNode, FileKind, FileNode,
    MaterializationScope, Metadata, MetadataChanges, Node, OpenFile, OpenOptions, RemoveKind,
    ResourceFactory, VfsComponent, VfsError, VfsPath, VfsPathBuf, VfsPathUnit, VfsResult, Volume,
};
use asar::{AsarReader, Header};
use memmap2::MmapOptions;
use std::{
    collections::{BTreeMap, HashMap},
    fs::File,
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

type Bytes = Arc<dyn AsRef<[u8]> + Send + Sync>;

enum AsarSource {
    File(PathBuf),
    Memory(Bytes),
}

pub struct AsarVolumeFactory {
    source: AsarSource,
    root: Vec<String>,
}

impl AsarVolumeFactory {
    pub fn new(path: PathBuf) -> Self {
        Self {
            source: AsarSource::File(path),
            root: Vec::new(),
        }
    }

    /// An archive held in memory, whose volume is rooted at the `/`-separated directory
    /// `root`, or the archive root when it is empty.
    pub fn memory(bytes: Arc<[u8]>, root: &str) -> Self {
        Self {
            source: AsarSource::Memory(Arc::new(bytes)),
            root: root
                .split('/')
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
                .collect(),
        }
    }
}

impl ResourceFactory<dyn Volume> for AsarVolumeFactory {
    fn materialize_in(&self, _scope: &mut MaterializationScope) -> VfsResult<Arc<dyn Volume>> {
        let mapping: Bytes = match &self.source {
            AsarSource::File(path) => {
                let file = File::open(path)?;
                // SAFETY: the read-only mapping owns its operating-system mapping handle.
                Arc::new(unsafe { MmapOptions::new().map(&file)? })
            }
            AsarSource::Memory(bytes) => bytes.clone(),
        };
        let data = (*mapping).as_ref();
        let (header, offset) = Header::read(&mut &data[..])?;
        validate_header(&header)?;
        let reader = AsarReader::new_from_header(header, offset, data, None)?;
        Ok(Arc::new(AsarVolume::from_reader(mapping.clone(), &reader)?.at(&self.root)?))
    }
}

fn validate_header(header: &Header) -> VfsResult<()> {
    match header {
        Header::File(file) if file.unpacked() => {
            Err(VfsError::Asar("unpacked entries are not supported".into()))
        }
        Header::File(_) => Ok(()),
        Header::Link { .. } => Err(VfsError::Asar("symbolic links are not supported".into())),
        Header::Directory { files } => {
            for (name, child) in files {
                VfsComponent::new(name)?;
                validate_header(child)?;
            }
            Ok(())
        }
    }
}

struct AsarVolume {
    inner: Arc<AsarInner>,
    root: usize,
}

struct AsarInner {
    mapping: Bytes,
    nodes: HashMap<usize, AsarNode>,
}

enum AsarNode {
    Directory(BTreeMap<Vec<VfsPathUnit>, (VfsComponent, usize)>),
    File { range: Range<usize> },
}

struct AsarDirectory {
    archive: Arc<AsarInner>,
    node: usize,
}

struct AsarFile {
    archive: Arc<AsarInner>,
    node: usize,
}

impl AsarVolume {
    const ROOT: usize = 0;

    fn from_reader(mapping: Bytes, reader: &AsarReader<'_>) -> VfsResult<Self> {
        let mut builder = AsarBuilder {
            nodes: HashMap::from([(Self::ROOT, AsarNode::Directory(BTreeMap::new()))]),
            paths: BTreeMap::from([(Vec::new(), Self::ROOT)]),
            next_node: Self::ROOT + 1,
        };

        let mut directories = reader.directories().keys().collect::<Vec<_>>();
        directories.sort_by_key(|path| path.components().count());
        for path in directories {
            builder.ensure_directory(path)?;
        }

        for (path, file) in reader.files() {
            let components = archive_components(path)?;
            let (name, parent_components) = components
                .split_last()
                .ok_or_else(|| VfsError::Asar("archive file has no name".into()))?;
            let parent = builder.ensure_components(parent_components)?;
            let normalized = name.normalized()?;
            let mut path_key = builder.key_for(parent)?;
            path_key.push(normalized.clone());
            if builder.paths.contains_key(&path_key) {
                return Err(VfsError::NameCollision(archive_vfs_path(path)?));
            }
            let node = builder.allocate(AsarNode::File {
                range: data_range((*mapping).as_ref(), file.data())?,
            });
            builder.paths.insert(path_key, node);
            let AsarNode::Directory(children) = builder.nodes.get_mut(&parent).unwrap() else {
                unreachable!()
            };
            children.insert(normalized, (name.clone(), node));
        }

        Ok(Self {
            inner: Arc::new(AsarInner {
                mapping,
                nodes: builder.nodes,
            }),
            root: Self::ROOT,
        })
    }

    /// The same archive, rooted at the directory `path` below the archive root.
    fn at(mut self, path: &[String]) -> VfsResult<Self> {
        for name in path {
            let AsarNode::Directory(children) =
                self.inner.nodes.get(&self.root).ok_or(VfsError::StaleNode)?
            else {
                return Err(VfsError::NotDirectory(VfsPathBuf::root()));
            };
            self.root = children
                .get(&VfsComponent::new(name)?.normalized()?)
                .map(|(_, child)| *child)
                .ok_or_else(|| VfsError::Asar(format!("archive has no directory '{}'", path.join("/"))))?;
        }
        Ok(self)
    }
}

struct AsarBuilder {
    nodes: HashMap<usize, AsarNode>,
    paths: BTreeMap<Vec<Vec<VfsPathUnit>>, usize>,
    next_node: usize,
}

impl AsarBuilder {
    fn allocate(&mut self, value: AsarNode) -> usize {
        let node = self.next_node;
        self.next_node = self
            .next_node
            .checked_add(1)
            .expect("ASAR node space exhausted");
        self.nodes.insert(node, value);
        node
    }

    fn key_for(&self, node: usize) -> VfsResult<Vec<Vec<VfsPathUnit>>> {
        self.paths
            .iter()
            .find_map(|(path, candidate)| (*candidate == node).then(|| path.clone()))
            .ok_or(VfsError::StaleNode)
    }

    fn ensure_directory(&mut self, path: &Path) -> VfsResult<usize> {
        self.ensure_components(&archive_components(path)?)
    }

    fn ensure_components(&mut self, components: &[VfsComponent]) -> VfsResult<usize> {
        let mut current = AsarVolume::ROOT;
        let mut path = Vec::new();
        for name in components {
            let normalized = name.normalized()?;
            path.push(normalized.clone());
            if let Some(node) = self.paths.get(&path) {
                if !matches!(self.nodes.get(node), Some(AsarNode::Directory(_))) {
                    return Err(VfsError::NameCollision(VfsPathBuf::root()));
                }
                current = *node;
                continue;
            }
            let node = self.allocate(AsarNode::Directory(BTreeMap::new()));
            self.paths.insert(path.clone(), node);
            let AsarNode::Directory(children) = self.nodes.get_mut(&current).unwrap() else {
                unreachable!()
            };
            if children.insert(normalized, (name.clone(), node)).is_some() {
                return Err(VfsError::NameCollision(VfsPathBuf::root()));
            }
            current = node;
        }
        Ok(current)
    }
}

impl AsarInner {
    fn node(self: &Arc<Self>, node: usize) -> VfsResult<Node> {
        match self.nodes.get(&node).ok_or(VfsError::StaleNode)? {
            AsarNode::Directory(_) => Ok(Node::Directory(Arc::new(AsarDirectory {
                archive: self.clone(),
                node,
            }))),
            AsarNode::File { .. } => Ok(Node::File(Arc::new(AsarFile {
                archive: self.clone(),
                node,
            }))),
        }
    }
}

impl Volume for AsarVolume {
    fn root(&self) -> VfsResult<Arc<dyn DirectoryNode>> {
        Ok(Arc::new(AsarDirectory {
            archive: self.inner.clone(),
            node: self.root,
        }))
    }

    fn rename(&self, _source: &VfsPath, _target: &VfsPath) -> VfsResult<()> {
        readonly()
    }
}

impl DirectoryNode for AsarDirectory {
    fn metadata(&self) -> VfsResult<Metadata> {
        Ok(Metadata {
            kind: FileKind::Directory,
            len: 0,
            readonly: true,
            sparse: false,
        })
    }

    fn set_metadata(&self, _changes: MetadataChanges) -> VfsResult<()> {
        readonly()
    }

    fn lookup(&self, name: &VfsComponent) -> VfsResult<Option<Node>> {
        let AsarNode::Directory(children) = self
            .archive
            .nodes
            .get(&self.node)
            .ok_or(VfsError::StaleNode)?
        else {
            return Err(VfsError::NotDirectory(VfsPathBuf::root()));
        };
        children
            .get(&name.normalized()?)
            .map(|(_, child)| self.archive.node(*child))
            .transpose()
    }

    fn read_dir(&self) -> VfsResult<Box<DirectoryIterator>> {
        let AsarNode::Directory(children) = self
            .archive
            .nodes
            .get(&self.node)
            .ok_or(VfsError::StaleNode)?
        else {
            return Err(VfsError::NotDirectory(VfsPathBuf::root()));
        };
        let entries = children
            .values()
            .map(|(name, child)| {
                Ok(DirectoryEntry {
                    name: name.clone(),
                    metadata: self.archive.node(*child)?.metadata()?,
                })
            })
            .collect::<Vec<_>>();
        Ok(Box::new(entries.into_iter()))
    }

    fn create_file(
        &self,
        _name: &VfsComponent,
        _options: CreateOptions,
    ) -> VfsResult<Arc<dyn FileNode>> {
        readonly()
    }

    fn create_dir(&self, _name: &VfsComponent) -> VfsResult<Arc<dyn DirectoryNode>> {
        readonly()
    }

    fn remove(&self, _name: &VfsComponent, _kind: RemoveKind) -> VfsResult<()> {
        readonly()
    }
}

impl FileNode for AsarFile {
    fn metadata(&self) -> VfsResult<Metadata> {
        let AsarNode::File { range, .. } = self
            .archive
            .nodes
            .get(&self.node)
            .ok_or(VfsError::StaleNode)?
        else {
            return Err(VfsError::IsDirectory(VfsPathBuf::root()));
        };
        Ok(Metadata {
            kind: FileKind::File,
            len: range.len() as u64,
            readonly: true,
            sparse: false,
        })
    }

    fn set_metadata(&self, _changes: MetadataChanges) -> VfsResult<()> {
        readonly()
    }

    fn open(&self, options: OpenOptions) -> VfsResult<Box<dyn OpenFile>> {
        if options.write || options.truncate {
            return readonly();
        }
        let AsarNode::File { range, .. } = self
            .archive
            .nodes
            .get(&self.node)
            .ok_or(VfsError::StaleNode)?
        else {
            return Err(VfsError::IsDirectory(VfsPathBuf::root()));
        };
        Ok(Box::new(AsarOpen {
            mapping: self.archive.mapping.clone(),
            range: range.clone(),
        }))
    }
}

struct AsarOpen {
    mapping: Bytes,
    range: Range<usize>,
}

impl OpenFile for AsarOpen {
    fn metadata(&self) -> VfsResult<Metadata> {
        Ok(Metadata {
            kind: FileKind::File,
            len: self.range.len() as u64,
            readonly: true,
            sparse: false,
        })
    }

    fn read(&self, offset: u64, buffer: &mut [u8]) -> VfsResult<usize> {
        let offset = usize::try_from(offset)
            .map_err(|_| VfsError::Unsupported("file offset exceeds host address space".into()))?;
        if offset >= self.range.len() {
            return Ok(0);
        }
        let count = buffer.len().min(self.range.len() - offset);
        let start = self.range.start + offset;
        buffer[..count].copy_from_slice(&(*self.mapping).as_ref()[start..start + count]);
        Ok(count)
    }

    fn write(&self, _offset: u64, _buffer: &[u8]) -> VfsResult<usize> {
        readonly()
    }

    fn truncate(&self, _len: u64) -> VfsResult<()> {
        readonly()
    }

    fn flush(&self) -> VfsResult<()> {
        Ok(())
    }

    fn sync(&self) -> VfsResult<()> {
        Ok(())
    }
}

fn data_range(mapping: &[u8], data: &[u8]) -> VfsResult<Range<usize>> {
    if data.is_empty() {
        return Ok(0..0);
    }
    let base = mapping.as_ptr() as usize;
    let start = (data.as_ptr() as usize)
        .checked_sub(base)
        .ok_or_else(|| VfsError::Asar("file data is outside archive mapping".into()))?;
    let end = start
        .checked_add(data.len())
        .ok_or_else(|| VfsError::Asar("file range overflows address space".into()))?;
    if end > mapping.len() {
        return Err(VfsError::Asar(
            "file data is outside archive mapping".into(),
        ));
    }
    Ok(start..end)
}

fn archive_components(path: &Path) -> VfsResult<Vec<VfsComponent>> {
    path.iter()
        .map(|name| {
            name.to_str()
                .ok_or_else(|| VfsError::Asar("archive path is not Unicode".into()))
                .and_then(VfsComponent::new)
        })
        .collect()
}

fn archive_vfs_path(path: &Path) -> VfsResult<VfsPathBuf> {
    let mut value = String::from("/");
    value.push_str(
        path.to_str()
            .ok_or_else(|| VfsError::Asar("archive path is not Unicode".into()))?
            .replace('\\', "/")
            .as_str(),
    );
    VfsPathBuf::new(value)
}

fn readonly<T>() -> VfsResult<T> {
    Err(VfsError::PermissionDenied(VfsPathBuf::root()))
}
