use crate::VfsPathBuf;
use thiserror::Error;

pub type VfsResult<T> = Result<T, VfsError>;

#[derive(Debug, Error)]
pub enum VfsError {
    #[error("invalid VFS path '{path}': {reason}")]
    InvalidPath { path: String, reason: String },
    #[error("invalid permission mode '{0}'")]
    InvalidPermissions(String),
    #[error("permission denied at '{0}'")]
    PermissionDenied(VfsPathBuf),
    #[error("filesystem entry was not found: '{0}'")]
    NotFound(VfsPathBuf),
    #[error("filesystem entry already exists: '{0}'")]
    AlreadyExists(VfsPathBuf),
    #[error("expected a directory at '{0}'")]
    NotDirectory(VfsPathBuf),
    #[error("expected a file at '{0}'")]
    IsDirectory(VfsPathBuf),
    #[error("directory is not empty: '{0}'")]
    DirectoryNotEmpty(VfsPathBuf),
    #[error("cannot place a binding below file binding '{0}'")]
    BindingBelowFile(VfsPathBuf),
    #[error("duplicate filesystem binding at '{0}'")]
    DuplicateBinding(VfsPathBuf),
    #[error("filesystem names collide under target path semantics at '{0}'")]
    NameCollision(VfsPathBuf),
    #[error("operation crosses mounted resources")]
    CrossDevice,
    #[error("resource authority {authority} does not permit binding mode {requested}")]
    AuthorityExceeded {
        authority: crate::Permissions,
        requested: crate::Permissions,
    },
    #[error("filesystem node no longer exists")]
    StaleNode,
    #[error("unsupported filesystem operation: {0}")]
    Unsupported(String),
    #[error("malformed ASAR archive: {0}")]
    Asar(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl VfsError {
    pub(crate) fn require_writable(writable: bool) -> VfsResult<()> {
        if writable {
            Ok(())
        } else {
            Err(Self::PermissionDenied(VfsPathBuf::root()))
        }
    }

    pub(crate) fn invalid_path(units: &[crate::VfsPathUnit], reason: impl Into<String>) -> Self {
        Self::InvalidPath {
            path: crate::Native::units_to_string_lossy(units),
            reason: reason.into(),
        }
    }
}

impl From<asar::Error> for VfsError {
    fn from(value: asar::Error) -> Self {
        Self::Asar(value.to_string())
    }
}

impl From<polymer_consolestorage::InvalidPermissions> for VfsError {
    fn from(value: polymer_consolestorage::InvalidPermissions) -> Self {
        Self::InvalidPermissions(value.0)
    }
}

impl From<polymer_consolestorage::Error> for VfsError {
    fn from(error: polymer_consolestorage::Error) -> Self {
        match error {
            polymer_consolestorage::Error::InvalidPath(path) => Self::InvalidPath {
                path,
                reason: "outside the mounted storage".to_owned(),
            },
            polymer_consolestorage::Error::Io { source, .. } => Self::Io(source),
            other => Self::Unsupported(other.to_string()),
        }
    }
}
