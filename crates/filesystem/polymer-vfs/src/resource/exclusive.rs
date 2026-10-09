use super::{FinalizationReason, ResourceLease};
use crate::{VfsError, VfsResult};
use dashmap::DashSet;
use parking_lot::Mutex;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
    },
};
use uuid::Uuid;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExclusiveResourceKey {
    HostPath(PathBuf),
    Resource(Uuid),
}

impl ExclusiveResourceKey {
    pub fn acquire(self) -> VfsResult<Arc<dyn ResourceLease>> {
        ExclusiveResourceLease::acquire(self)
    }

    pub fn is_held(&self) -> bool {
        match self {
            Self::Resource(id) => ACTIVE_RESOURCES.contains(id),
            Self::HostPath(path) => path.canonicalize().is_ok_and(|canonical| {
                let key = normalize_host_path(&canonical);
                ACTIVE_HOST_PATHS
                    .lock()
                    .iter()
                    .any(|existing| path_keys_overlap(existing, &key))
            }),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ActiveResourceKey {
    HostPath(String),
    Resource(Uuid),
}

static ACTIVE_RESOURCES: LazyLock<DashSet<Uuid>> = LazyLock::new(DashSet::new);
static ACTIVE_HOST_PATHS: LazyLock<Mutex<Vec<String>>> = LazyLock::new(Mutex::default);

struct ExclusiveResourceLease {
    key: ActiveResourceKey,
    released: AtomicBool,
}

impl ExclusiveResourceLease {
    fn acquire(key: ExclusiveResourceKey) -> VfsResult<Arc<dyn ResourceLease>> {
        let display = format!("{key:?}");
        let overlap = |existing: ActiveResourceKey| {
            VfsError::Unsupported(format!(
                "resource {display} overlaps the active resource {existing:?}"
            ))
        };
        let key = match key {
            ExclusiveResourceKey::Resource(id) => {
                if !ACTIVE_RESOURCES.insert(id) {
                    return Err(overlap(ActiveResourceKey::Resource(id)));
                }
                ActiveResourceKey::Resource(id)
            }
            ExclusiveResourceKey::HostPath(path) => {
                let key = normalize_host_path(&path.canonicalize()?);
                let mut active = ACTIVE_HOST_PATHS.lock();
                if let Some(existing) = active
                    .iter()
                    .find(|existing| path_keys_overlap(existing, &key))
                {
                    return Err(overlap(ActiveResourceKey::HostPath(existing.clone())));
                }
                active.push(key.clone());
                ActiveResourceKey::HostPath(key)
            }
        };
        Ok(Arc::new(Self {
            key,
            released: AtomicBool::new(false),
        }))
    }

    fn release(&self) {
        if self.released.swap(true, Ordering::AcqRel) {
            return;
        }
        match &self.key {
            ActiveResourceKey::Resource(id) => {
                ACTIVE_RESOURCES.remove(id);
            }
            ActiveResourceKey::HostPath(path) => {
                ACTIVE_HOST_PATHS.lock().retain(|key| key != path);
            }
        }
    }
}

impl ResourceLease for ExclusiveResourceLease {
    fn finalize(&self, _reason: FinalizationReason) -> VfsResult<()> {
        self.release();
        Ok(())
    }
}

impl Drop for ExclusiveResourceLease {
    fn drop(&mut self) {
        self.release();
    }
}

fn normalize_host_path(path: &Path) -> String {
    let normalized = path.to_string_lossy().replace('/', "\\");
    #[cfg(windows)]
    {
        normalized.to_lowercase()
    }
    #[cfg(not(windows))]
    {
        normalized
    }
}

fn path_keys_overlap(left: &str, right: &str) -> bool {
    fn contains(parent: &str, child: &str) -> bool {
        child == parent
            || child
                .strip_prefix(parent)
                .is_some_and(|suffix| suffix.starts_with('\\'))
    }
    contains(left, right) || contains(right, left)
}
