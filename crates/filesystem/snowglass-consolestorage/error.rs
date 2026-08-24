use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("`{root}` is not a valid depot, missing: {}", missing.join(", "))]
    InvalidDepot {
        /// The root the layout was checked against.
        root: String,
        /// The required entries that are absent, or present with the wrong kind.
        ///
        /// Spelled as declared, i.e. directories keep their trailing `/`.
        missing: Vec<String>,
    },
    #[error("`{0}` is not a valid storage path")]
    InvalidPath(String),
    #[error("i/o error at `{path}`")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("`{file}` is malformed: {reason}")]
    MalformedMetadata { file: String, reason: String },
    #[error("artifact database: {reason}")]
    Database { reason: String },
    #[error("nand has no identity")]
    UnclaimedNand,
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("installation did not run to completion")]
    InstallIncomplete,
    /// The file in the depot is corrupt.
    #[error("corrupt depot")]
    IntegrityMatchFailed { failed: String },
    #[error("time error")]
    TimeError(#[from] jiff::Error),
    #[error("git history {operation}: {reason}")]
    Git {
        operation: &'static str,
        reason: String,
    },
    #[error("save history conflict: {0}")]
    HistoryConflict(String),
    #[error("save commit `{0}` was not found in this save's history")]
    UnknownSaveCommit(String),
}

impl Clone for Error {
    fn clone(&self) -> Self {
        match self {
            Error::InvalidDepot { root, missing } => Error::InvalidDepot {
                root: root.clone(),
                missing: missing.clone(),
            },
            Error::InvalidPath(path) => Error::InvalidPath(path.clone()),
            Error::Io { path, source } => Error::Io {
                path: path.clone(),
                source: std::io::Error::new(source.kind(), source.to_string()),
            },
            Error::MalformedMetadata { file, reason } => Error::MalformedMetadata {
                file: file.clone(),
                reason: reason.clone(),
            },
            Error::Database { reason } => Error::Database {
                reason: reason.clone(),
            },
            Error::UnclaimedNand => Error::UnclaimedNand,
            Error::Unsupported(what) => Error::Unsupported(what.clone()),
            Error::InstallIncomplete => Error::InstallIncomplete,
            Error::IntegrityMatchFailed { failed } => Error::IntegrityMatchFailed {
                failed: failed.clone(),
            },
            Error::TimeError(failure) => Error::TimeError(failure.clone()),
            Error::Git { operation, reason } => Error::Git {
                operation,
                reason: reason.clone(),
            },
            Error::HistoryConflict(reason) => Error::HistoryConflict(reason.clone()),
            Error::UnknownSaveCommit(id) => Error::UnknownSaveCommit(id.clone()),
        }
    }
}

impl Error {
    pub fn malformed(file: impl Into<String>, reason: impl Into<String>) -> Self {
        Error::MalformedMetadata {
            file: file.into(),
            reason: reason.into(),
        }
    }

    pub fn git(operation: &'static str, error: impl std::fmt::Display) -> Self {
        Error::Git {
            operation,
            reason: error.to_string(),
        }
    }

    pub fn io(path: &Path, source: std::io::Error) -> Self {
        Error::Io {
            path: path.display().to_string(),
            source,
        }
    }

    pub fn not_found(path: &str) -> Self {
        Error::Io {
            path: path.to_owned(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "entry not found"),
        }
    }
}
