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
    missing: Vec<String>
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
  MalformedMetadata {
    file: String,
    reason: String,
  },
  #[error("artifact database: {reason}")]
  Database {
    reason: String,
  },
  #[error("nand has no identity")]
  UnclaimedNand,
  #[error("installation did not run to completion")]
  InstallIncomplete,
  /// The file in the depot is corrupt.
  #[error("corrupt depot")]
  IntegrityMatchFailed {
    failed: String,
  },
  #[error("time error")]
  TimeError(#[from] jiff::Error)
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
      Error::InstallIncomplete => Error::InstallIncomplete,
      Error::IntegrityMatchFailed { failed } => Error::IntegrityMatchFailed {
        failed: failed.clone(),
      },
      Error::TimeError(failure) => Error::TimeError(failure.clone())
    }
  }
}

impl Error {
  pub fn io(path: &Path, source: std::io::Error) -> Self {
    Error::Io {
      path: path.display().to_string(),
      source,
    }
  }
}
