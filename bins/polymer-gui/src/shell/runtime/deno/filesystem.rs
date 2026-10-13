use deno_core::{OpState, op2};
use deno_error::JsErrorBox;
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    io::ErrorKind,
    path::{Path, PathBuf},
    rc::Rc,
};

use crate::shell::document::SharedShellDocument;

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum PickKind {
    Open,
    Save,
    Directory,
}

#[derive(Deserialize)]
pub(super) struct PickFilter {
    description: String,
    extensions: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PickRequest {
    kind: PickKind,
    multiple: bool,
    suggested_name: Option<String>,
    start_in: Option<String>,
    filters: Vec<PickFilter>,
}

#[derive(Serialize)]
pub(super) struct DirectoryEntry {
    name: String,
    kind: &'static str,
}

/// Host file access behind the File System Access API. Script only reaches
/// these through handles the user picked, or entries beneath them.
struct HostFiles;

impl HostFiles {
    fn error(error: std::io::Error, path: &Path) -> JsErrorBox {
        let class = match error.kind() {
            ErrorKind::NotFound => "DOMExceptionNotFoundError",
            ErrorKind::PermissionDenied => "DOMExceptionNotAllowedError",
            ErrorKind::DirectoryNotEmpty => "DOMExceptionInvalidModificationError",
            _ => return JsErrorBox::generic(format!("{}: {error}", path.display())),
        };
        JsErrorBox::new(class, format!("{}: {error}", path.display()))
    }

    fn child(directory: &str, name: &str) -> Result<PathBuf, JsErrorBox> {
        let mut components = Path::new(name).components();
        let single = matches!(
            (components.next(), components.next()),
            (Some(std::path::Component::Normal(_)), None)
        );
        if !single || name.contains([':', '/', '\\']) {
            return Err(JsErrorBox::type_error(format!(
                "`{name}` is not a valid entry name"
            )));
        }
        Ok(Path::new(directory).join(name))
    }

    async fn blocking<T: Send + 'static>(
        work: impl FnOnce() -> Result<T, JsErrorBox> + Send + 'static,
    ) -> Result<T, JsErrorBox> {
        tokio::task::spawn_blocking(work)
            .await
            .map_err(JsErrorBox::from_err)?
    }
}

#[op2]
#[serde]
pub(super) async fn op_fs_pick(
    state: Rc<RefCell<OpState>>,
    #[serde] request: PickRequest,
) -> Result<Vec<String>, JsErrorBox> {
    let parent = state
        .borrow()
        .borrow::<SharedShellDocument>()
        .0
        .borrow()
        .dialog_parent();
    let mut dialog = rfd::AsyncFileDialog::new();
    if let Ok(Some(parent)) = parent.await {
        dialog = dialog.set_parent(&parent);
    }
    for filter in &request.filters {
        dialog = dialog.add_filter(&filter.description, &filter.extensions);
    }
    if let Some(directory) = request.start_in {
        dialog = dialog.set_directory(directory);
    }
    if let Some(name) = request.suggested_name {
        dialog = dialog.set_file_name(name);
    }
    let picked = match (request.kind, request.multiple) {
        (PickKind::Open, false) => dialog.pick_file().await.map(|file| vec![file]),
        (PickKind::Open, true) => dialog.pick_files().await,
        (PickKind::Save, _) => match dialog.save_file().await {
            Some(file) => {
                let path = file.path().to_owned();
                HostFiles::blocking(move || {
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&path)
                        .map(drop)
                        .map_err(|error| HostFiles::error(error, &path))
                })
                .await?;
                Some(vec![file])
            }
            None => None,
        },
        (PickKind::Directory, false) => dialog.pick_folder().await.map(|folder| vec![folder]),
        (PickKind::Directory, true) => dialog.pick_folders().await,
    };
    Ok(picked
        .unwrap_or_default()
        .into_iter()
        .map(|handle| handle.path().to_string_lossy().into_owned())
        .collect())
}

/// Resolves `name` inside `directory` as an entry of `kind`, creating it when asked.
#[op2]
#[string]
pub(super) async fn op_fs_child(
    #[string] directory: String,
    #[string] name: String,
    #[string] kind: String,
    create: bool,
) -> Result<String, JsErrorBox> {
    let path = HostFiles::child(&directory, &name)?;
    HostFiles::blocking(move || {
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_dir() == (kind == "directory") => {}
            Ok(_) => {
                return Err(JsErrorBox::new(
                    "DOMExceptionTypeMismatchError",
                    format!("{} is not a {kind}", path.display()),
                ));
            }
            Err(error) if error.kind() == ErrorKind::NotFound && create => {
                let created = match kind.as_str() {
                    "directory" => std::fs::create_dir(&path),
                    _ => std::fs::write(&path, []),
                };
                created.map_err(|error| HostFiles::error(error, &path))?;
            }
            Err(error) => return Err(HostFiles::error(error, &path)),
        }
        Ok(path.to_string_lossy().into_owned())
    })
    .await
}

#[op2]
#[serde]
pub(super) async fn op_fs_entries(
    #[string] directory: String,
) -> Result<Vec<DirectoryEntry>, JsErrorBox> {
    HostFiles::blocking(move || {
        let path = Path::new(&directory);
        let entries = std::fs::read_dir(path).map_err(|error| HostFiles::error(error, path))?;
        Ok(entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let kind = entry.file_type().ok()?;
                Some(DirectoryEntry {
                    name: entry.file_name().into_string().ok()?,
                    kind: if kind.is_dir() { "directory" } else { "file" },
                })
            })
            .collect())
    })
    .await
}

#[op2]
#[buffer]
pub(super) async fn op_fs_read(#[string] path: String) -> Result<Vec<u8>, JsErrorBox> {
    HostFiles::blocking(move || {
        std::fs::read(&path).map_err(|error| HostFiles::error(error, Path::new(&path)))
    })
    .await
}

/// Milliseconds since the Unix epoch.
#[op2]
pub(super) async fn op_fs_modified(#[string] path: String) -> Result<f64, JsErrorBox> {
    HostFiles::blocking(move || {
        let path = Path::new(&path);
        let modified = std::fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .map_err(|error| HostFiles::error(error, path))?;
        Ok(modified
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |elapsed| elapsed.as_millis() as f64))
    })
    .await
}

/// Replaces a file's contents in one step, as a writable stream's `close()` requires.
#[op2]
pub(super) async fn op_fs_write(
    #[string] path: String,
    #[buffer(copy)] bytes: Vec<u8>,
) -> Result<(), JsErrorBox> {
    HostFiles::blocking(move || {
        let path = PathBuf::from(path);
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy())
            .unwrap_or_default();
        let swap = path.with_file_name(format!(".{name}.crswap"));
        std::fs::write(&swap, bytes)
            .and_then(|()| std::fs::rename(&swap, &path))
            .map_err(|error| {
                let _ = std::fs::remove_file(&swap);
                HostFiles::error(error, &path)
            })
    })
    .await
}

#[op2]
pub(super) async fn op_fs_remove(
    #[string] directory: String,
    #[string] name: String,
    recursive: bool,
) -> Result<(), JsErrorBox> {
    let path = HostFiles::child(&directory, &name)?;
    HostFiles::blocking(move || {
        let metadata = std::fs::metadata(&path).map_err(|error| HostFiles::error(error, &path))?;
        let removed = match (metadata.is_dir(), recursive) {
            (true, true) => std::fs::remove_dir_all(&path),
            (true, false) => std::fs::remove_dir(&path),
            (false, _) => std::fs::remove_file(&path),
        };
        removed.map_err(|error| HostFiles::error(error, &path))
    })
    .await
}
