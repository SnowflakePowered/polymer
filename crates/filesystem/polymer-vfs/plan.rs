use crate::{
    FileKind, FileNode, Permissions, PreparedFilesystem, Resource, ResourceOrigin,
    TempDirectoryFactory, VfsComponent, VfsError, VfsPathBuf, VfsPathUnit, VfsResult, Volume,
};
use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
};

#[derive(Clone, Debug)]
pub enum BindingKind {
    Tree(Arc<Resource<dyn Volume>>),
    File(Arc<Resource<dyn FileNode>>),
    Storage {
        resource: Arc<Resource<dyn Volume>>,
        format: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub enum BindingSpec {
    Tree {
        at: VfsPathBuf,
        resource: Arc<Resource<dyn Volume>>,
        permissions: Permissions,
    },
    File {
        at: VfsPathBuf,
        resource: Arc<Resource<dyn FileNode>>,
        permissions: Permissions,
    },
    Storage {
        at: VfsPathBuf,
        resource: Arc<Resource<dyn Volume>>,
        permissions: Permissions,
        format: Option<String>,
    },
}

impl BindingSpec {
    pub fn at(&self) -> &VfsPathBuf {
        match self {
            Self::Tree { at, .. } | Self::File { at, .. } | Self::Storage { at, .. } => at,
        }
    }

    pub const fn permissions(&self) -> Permissions {
        match self {
            Self::Tree { permissions, .. }
            | Self::File { permissions, .. }
            | Self::Storage { permissions, .. } => *permissions,
        }
    }

    pub fn kind(&self) -> BindingKind {
        match self {
            Self::Tree { resource, .. } => BindingKind::Tree(resource.clone()),
            Self::File { resource, .. } => BindingKind::File(resource.clone()),
            Self::Storage {
                resource, format, ..
            } => BindingKind::Storage {
                resource: resource.clone(),
                format: format.clone(),
            },
        }
    }
}

#[derive(Debug, Default)]
pub struct FilesystemPlan {
    bindings: Vec<BindingSpec>,
}

impl FilesystemPlan {
    pub fn mount_tree(
        &mut self,
        at: VfsPathBuf,
        resource: Arc<Resource<dyn Volume>>,
        permissions: Permissions,
    ) {
        self.bindings.push(BindingSpec::Tree {
            at,
            resource,
            permissions,
        });
    }

    pub fn mount_file(
        &mut self,
        at: VfsPathBuf,
        resource: Arc<Resource<dyn FileNode>>,
        permissions: Permissions,
    ) {
        self.bindings.push(BindingSpec::File {
            at,
            resource,
            permissions,
        });
    }

    pub fn mount_storage(
        &mut self,
        at: VfsPathBuf,
        resource: Arc<Resource<dyn Volume>>,
        permissions: Permissions,
        format: Option<String>,
    ) {
        self.bindings.push(BindingSpec::Storage {
            at,
            resource,
            permissions,
            format,
        });
    }

    pub fn bindings(&self) -> &[BindingSpec] {
        &self.bindings
    }

    pub fn seal(self) -> VfsResult<PreparedFilesystem> {
        let compiled = CompiledFilesystem::compile(self)?;
        Ok(PreparedFilesystem::new(compiled))
    }
}

#[derive(Clone, Debug)]
pub struct Binding {
    at: VfsPathBuf,
    normalized: Vec<Vec<VfsPathUnit>>,
    kind: BindingKind,
    permissions: Permissions,
}

impl Binding {
    pub fn at(&self) -> &VfsPathBuf {
        &self.at
    }

    pub fn kind(&self) -> &BindingKind {
        &self.kind
    }

    pub const fn permissions(&self) -> Permissions {
        self.permissions
    }

    pub(crate) fn depth(&self) -> usize {
        self.normalized.len()
    }
}

#[derive(Debug)]
pub struct CompiledFilesystem {
    bindings: Vec<Binding>,
}

impl CompiledFilesystem {
    fn compile(plan: FilesystemPlan) -> VfsResult<Self> {
        let mut indexed = BTreeMap::<Vec<Vec<VfsPathUnit>>, Binding>::new();
        let mut storages = HashSet::new();
        for spec in plan.bindings {
            if let BindingKind::Storage { resource, .. } = spec.kind()
                && !storages.insert(Arc::as_ptr(&resource) as *const ())
            {
                return Err(VfsError::Unsupported(format!(
                    "storage mounted at {} is already mounted",
                    spec.at().to_string_lossy()
                )));
            }
            let normalized = spec.at().normalized_components()?;
            let authority = match spec.kind() {
                BindingKind::Tree(resource) => resource.authority(),
                BindingKind::File(resource) => resource.authority(),
                BindingKind::Storage { resource, .. } => resource.authority(),
            };
            if !authority.contains(spec.permissions()) {
                return Err(VfsError::AuthorityExceeded {
                    authority,
                    requested: spec.permissions(),
                });
            }
            if let Some(existing) = indexed.get(&normalized) {
                return Err(if existing.at == *spec.at() {
                    VfsError::DuplicateBinding(spec.at().clone())
                } else {
                    VfsError::NameCollision(spec.at().clone())
                });
            }
            indexed.insert(
                normalized.clone(),
                Binding {
                    at: spec.at().clone(),
                    normalized,
                    kind: spec.kind(),
                    permissions: spec.permissions(),
                },
            );
        }

        for (path, binding) in &indexed {
            for depth in 0..path.len() {
                if indexed
                    .get(&path[..depth])
                    .is_some_and(|parent| matches!(parent.kind, BindingKind::File(_)))
                {
                    return Err(VfsError::BindingBelowFile(binding.at.clone()));
                }
            }
        }

        Self::insert_scratch_parents(&mut indexed)?;
        let mut bindings = indexed.into_values().collect::<Vec<_>>();
        bindings.sort_by(|left, right| {
            left.normalized
                .len()
                .cmp(&right.normalized.len())
                .then_with(|| left.normalized.cmp(&right.normalized))
        });
        Ok(Self { bindings })
    }

    fn insert_scratch_parents(
        indexed: &mut BTreeMap<Vec<Vec<VfsPathUnit>>, Binding>,
    ) -> VfsResult<()> {
        let mut missing = BTreeMap::new();
        for binding in indexed.values() {
            let mut parent = binding.at.parent();
            while let Some(at) = parent {
                let normalized = at.normalized_components()?;
                // Explicit volumes own their whole subtree, including directories
                // supplied by the backend. Only fill gaps outside those mounts.
                let covered =
                    (0..=normalized.len()).any(|depth| indexed.contains_key(&normalized[..depth]));
                parent = at.parent();
                if !covered {
                    missing.entry(normalized).or_insert(at);
                }
            }
        }
        let permissions = Permissions::READ | Permissions::WRITE | Permissions::EXECUTE;
        for (normalized, at) in missing {
            indexed.insert(
                normalized.clone(),
                Binding {
                    at,
                    normalized,
                    kind: BindingKind::Tree(Arc::new(Resource::new(
                        Arc::new(TempDirectoryFactory::new()),
                        permissions,
                        ResourceOrigin::Scratch,
                    ))),
                    permissions,
                },
            );
        }
        Ok(())
    }

    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    pub fn resolve(&self, path: &VfsPathBuf) -> VfsResult<Option<&Binding>> {
        let normalized = path.normalized_components()?;
        Ok(self.bindings.iter().rev().find(|binding| {
            normalized.starts_with(&binding.normalized)
                && (matches!(
                    binding.kind,
                    BindingKind::Tree(_) | BindingKind::Storage { .. }
                ) || normalized.len() == binding.normalized.len())
        }))
    }

    pub(crate) fn has_descendant(&self, path: &VfsPathBuf) -> VfsResult<bool> {
        let normalized = path.normalized_components()?;
        Ok(self.bindings.iter().any(|binding| {
            binding.normalized.starts_with(&normalized)
                && binding.normalized.len() > normalized.len()
        }))
    }

    pub(crate) fn is_mount_point(&self, path: &VfsPathBuf) -> VfsResult<bool> {
        let normalized = path.normalized_components()?;
        Ok(self
            .bindings
            .iter()
            .any(|binding| binding.normalized == normalized))
    }

    pub(crate) fn mount_children(
        &self,
        path: &VfsPathBuf,
    ) -> VfsResult<Vec<(VfsComponent, FileKind)>> {
        let normalized = path.normalized_components()?;
        let mut children = BTreeMap::<Vec<VfsPathUnit>, (VfsComponent, FileKind)>::new();
        for binding in &self.bindings {
            if !binding.normalized.starts_with(&normalized)
                || binding.normalized.len() <= normalized.len()
            {
                continue;
            }
            let index = normalized.len();
            let name = binding
                .at
                .components()
                .nth(index)
                .expect("binding contains descendant component");
            let component = VfsComponent::from_units(crate::PathStr::as_slice(name).to_vec())?;
            let kind = if binding.normalized.len() == normalized.len() + 1 {
                match binding.kind {
                    BindingKind::Tree(_) => FileKind::Directory,
                    BindingKind::File(_) => FileKind::File,
                    // Storage roots are resolved after their volume is materialized.
                    BindingKind::Storage { .. } => FileKind::Directory,
                }
            } else {
                FileKind::Directory
            };
            children.insert(binding.normalized[index].clone(), (component, kind));
        }
        Ok(children.into_values().collect())
    }
}
