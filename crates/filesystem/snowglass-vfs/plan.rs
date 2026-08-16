use crate::{
    FileKind, FileNode, Permissions, PreparedFilesystem, Resource, VfsComponent, VfsError,
    VfsPathBuf, VfsPathUnit, VfsResult, Volume,
};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProfileLayout {
    pub home: Option<VfsPathBuf>,
    pub roaming_app_data: Option<VfsPathBuf>,
    pub local_app_data: Option<VfsPathBuf>,
    pub temp: Option<VfsPathBuf>,
}

#[derive(Clone, Debug)]
pub enum BindingKind {
    Tree(Arc<Resource<dyn Volume>>),
    File(Arc<Resource<dyn FileNode>>),
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
}

impl BindingSpec {
    pub fn at(&self) -> &VfsPathBuf {
        match self {
            Self::Tree { at, .. } | Self::File { at, .. } => at,
        }
    }

    pub const fn permissions(&self) -> Permissions {
        match self {
            Self::Tree { permissions, .. } | Self::File { permissions, .. } => *permissions,
        }
    }

    pub fn kind(&self) -> BindingKind {
        match self {
            Self::Tree { resource, .. } => BindingKind::Tree(resource.clone()),
            Self::File { resource, .. } => BindingKind::File(resource.clone()),
        }
    }
}

#[derive(Debug)]
pub struct FilesystemPlan {
    bindings: Vec<BindingSpec>,
    working_directory: VfsPathBuf,
    profile: Option<ProfileLayout>,
}

impl Default for FilesystemPlan {
    fn default() -> Self {
        Self {
            bindings: Vec::new(),
            working_directory: VfsPathBuf::root(),
            profile: None,
        }
    }
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

    pub fn set_working_directory(&mut self, path: VfsPathBuf) {
        self.working_directory = path;
    }

    pub fn set_profile(&mut self, profile: ProfileLayout) {
        self.profile = Some(profile);
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
    working_directory: VfsPathBuf,
    profile: Option<ProfileLayout>,
}

impl CompiledFilesystem {
    fn compile(plan: FilesystemPlan) -> VfsResult<Self> {
        let mut indexed = BTreeMap::<Vec<Vec<VfsPathUnit>>, Binding>::new();
        for spec in plan.bindings {
            let normalized = spec.at().normalized_components()?;
            let authority = match spec.kind() {
                BindingKind::Tree(resource) => resource.authority(),
                BindingKind::File(resource) => resource.authority(),
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

        let working = plan.working_directory.normalized_components()?;
        ensure_projected_directory(&working, &indexed, &plan.working_directory)?;
        if let Some(profile) = &plan.profile {
            for path in [
                profile.home.as_ref(),
                profile.roaming_app_data.as_ref(),
                profile.local_app_data.as_ref(),
                profile.temp.as_ref(),
            ]
            .into_iter()
            .flatten()
            {
                let normalized = path.normalized_components()?;
                let binding = resolve_binding(&normalized, &indexed)
                    .ok_or_else(|| VfsError::NotFound(path.clone()))?;
                if !binding.permissions.can_write() {
                    return Err(VfsError::PermissionDenied(path.clone()));
                }
            }
        }

        let mut bindings = indexed.into_values().collect::<Vec<_>>();
        bindings.sort_by(|left, right| {
            left.normalized
                .len()
                .cmp(&right.normalized.len())
                .then_with(|| left.normalized.cmp(&right.normalized))
        });
        Ok(Self {
            bindings,
            working_directory: plan.working_directory,
            profile: plan.profile,
        })
    }

    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    pub fn working_directory(&self) -> &VfsPathBuf {
        &self.working_directory
    }

    pub fn profile(&self) -> Option<&ProfileLayout> {
        self.profile.as_ref()
    }

    pub fn resolve(&self, path: &VfsPathBuf) -> VfsResult<Option<&Binding>> {
        let normalized = path.normalized_components()?;
        Ok(self.bindings.iter().rev().find(|binding| {
            normalized.starts_with(&binding.normalized)
                && (matches!(binding.kind, BindingKind::Tree(_))
                    || normalized.len() == binding.normalized.len())
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
                }
            } else {
                FileKind::Directory
            };
            children.insert(binding.normalized[index].clone(), (component, kind));
        }
        Ok(children.into_values().collect())
    }
}

fn resolve_binding<'a>(
    path: &[Vec<VfsPathUnit>],
    indexed: &'a BTreeMap<Vec<Vec<VfsPathUnit>>, Binding>,
) -> Option<&'a Binding> {
    (0..=path.len())
        .rev()
        .find_map(|depth| indexed.get(&path[..depth]))
        .filter(|binding| {
            matches!(binding.kind, BindingKind::Tree(_)) || binding.normalized.len() == path.len()
        })
}

fn ensure_projected_directory(
    path: &[Vec<VfsPathUnit>],
    indexed: &BTreeMap<Vec<Vec<VfsPathUnit>>, Binding>,
    original: &VfsPathBuf,
) -> VfsResult<()> {
    if path.is_empty() {
        return Ok(());
    }
    match resolve_binding(path, indexed) {
        Some(binding) if matches!(binding.kind, BindingKind::Tree(_)) => Ok(()),
        Some(_) => Err(VfsError::NotDirectory(original.clone())),
        None if indexed
            .keys()
            .any(|binding| binding.starts_with(path) && binding.len() > path.len()) =>
        {
            Ok(())
        }
        None => Err(VfsError::NotFound(original.clone())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MemoryFileFactory, ResourceOrigin};
    use std::sync::Arc;

    #[test]
    fn allows_individual_file_overlays_but_rejects_children_of_files() {
        let first = Arc::new(Resource::new(
            Arc::new(MemoryFileFactory::immutable(b"one".to_vec())),
            Permissions::READ,
            ResourceOrigin::Memory,
        ));
        let second = Arc::new(Resource::new(
            Arc::new(MemoryFileFactory::immutable(b"two".to_vec())),
            Permissions::READ,
            ResourceOrigin::Memory,
        ));
        let mut plan = FilesystemPlan::default();
        plan.mount_file(
            "/Emulator/config.ini".parse().unwrap(),
            first,
            Permissions::READ,
        );
        plan.mount_file(
            "/Emulator/config.ini/child".parse().unwrap(),
            second,
            Permissions::READ,
        );
        assert!(matches!(plan.seal(), Err(VfsError::BindingBelowFile(_))));
    }

    #[cfg(windows)]
    #[test]
    fn windows_bindings_detect_case_collisions() {
        let first = Arc::new(Resource::new(
            Arc::new(MemoryFileFactory::immutable(Vec::new())),
            Permissions::READ,
            ResourceOrigin::Memory,
        ));
        let second = Arc::new(Resource::new(
            Arc::new(MemoryFileFactory::immutable(Vec::new())),
            Permissions::READ,
            ResourceOrigin::Memory,
        ));
        let mut plan = FilesystemPlan::default();
        plan.mount_file("/Dolphin/a.ini".parse().unwrap(), first, Permissions::READ);
        plan.mount_file("/dolphin/A.ini".parse().unwrap(), second, Permissions::READ);
        assert!(matches!(plan.seal(), Err(VfsError::NameCollision(_))));
    }

    #[cfg(unix)]
    #[test]
    fn posix_bindings_remain_case_sensitive() {
        let first = Arc::new(Resource::new(
            Arc::new(MemoryFileFactory::immutable(Vec::new())),
            Permissions::READ,
            ResourceOrigin::Memory,
        ));
        let second = Arc::new(Resource::new(
            Arc::new(MemoryFileFactory::immutable(Vec::new())),
            Permissions::READ,
            ResourceOrigin::Memory,
        ));
        let mut plan = FilesystemPlan::default();
        plan.mount_file("/Dolphin/a.ini".parse().unwrap(), first, Permissions::READ);
        plan.mount_file("/dolphin/A.ini".parse().unwrap(), second, Permissions::READ);
        assert!(plan.seal().is_ok());
    }
}
