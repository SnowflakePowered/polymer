use crate::wire::{Attributes, Entry, Frame, MAX_IO, Request, Response};
use parking_lot::Mutex;
use polymer_ipc::xpc::{Connection, Listener};
use polymer_vfs::{
    CreateOptions, FileKind, MetadataChanges, OpenFile, OpenOptions, PreparedFilesystem,
    RemoveKind, VfsError, VfsPathBuf, VfsResult,
};
use std::{
    collections::HashMap,
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

pub(crate) struct Server {
    pub token: [u8; 32],
    listener: Listener,
    stopping: Arc<AtomicBool>,
    connection: Arc<Mutex<Option<Connection>>>,
    worker: Option<JoinHandle<()>>,
}
impl Server {
    pub fn start(filesystem: Arc<PreparedFilesystem>) -> io::Result<Self> {
        let token: [u8; 32] = rand::random();
        let name: String = token.iter().map(|byte| format!("{byte:02x}")).collect();
        let listener = Listener::bind(&name)?;
        let stopping = Arc::new(AtomicBool::new(false));
        let connection = Arc::new(Mutex::new(None));
        let stop = stopping.clone();
        let peer = connection.clone();
        let acceptor = listener.clone();
        let worker = thread::Builder::new()
            .name("polymer-fskit".into())
            .spawn(move || {
                while let Ok(connection) = acceptor.accept() {
                    *peer.lock() = Some(connection.clone());
                    if stop.load(Ordering::Acquire) {
                        connection.close();
                        break;
                    }
                    let mut session = Session {
                        filesystem: filesystem.clone(),
                        files: HashMap::new(),
                        next: 1,
                    };
                    let _ = session.serve(&connection);
                    connection.close();
                    peer.lock().take();
                }
            })?;
        Ok(Self {
            token,
            listener,
            stopping,
            connection,
            worker: Some(worker),
        })
    }
    fn errno(error: VfsError) -> i32 {
        match error {
            VfsError::Io(error) => error.raw_os_error().unwrap_or(libc::EIO),
            VfsError::NotFound(_) => libc::ENOENT,
            VfsError::AlreadyExists(_) => libc::EEXIST,
            VfsError::NotDirectory(_) => libc::ENOTDIR,
            VfsError::IsDirectory(_) => libc::EISDIR,
            VfsError::DirectoryNotEmpty(_) => libc::ENOTEMPTY,
            VfsError::PermissionDenied(_) | VfsError::AuthorityExceeded { .. } => libc::EACCES,
            VfsError::CrossDevice => libc::EXDEV,
            VfsError::StaleNode => libc::ESTALE,
            VfsError::Unsupported(_) => libc::ENOTSUP,
            _ => libc::EINVAL,
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        self.listener.close();
        if let Some(connection) = self.connection.lock().take() {
            connection.close();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
struct Session {
    filesystem: Arc<PreparedFilesystem>,
    files: HashMap<u64, Box<dyn OpenFile>>,
    next: u64,
}
impl Session {
    fn serve(&mut self, connection: &Connection) -> io::Result<()> {
        loop {
            let call = connection.receive_call(None)?;
            let request = Frame::decode(&call.data)?;
            let response = self
                .dispatch(request)
                .unwrap_or_else(|error| Response::Error(Server::errno(error)));
            call.reply(&Frame::encode(&response)?);
        }
    }

    fn attributes(&self, path: &VfsPathBuf) -> VfsResult<Attributes> {
        let metadata = self.filesystem.metadata(path)?;
        let permissions = self
            .filesystem
            .compiled()
            .resolve(path)?
            .map(|binding| binding.permissions());
        let directory = metadata.kind == FileKind::Directory;
        let readable = permissions.is_none_or(|p| p.can_read());
        let writable = !metadata.readonly && permissions.is_some_and(|p| p.can_write());
        let executable = directory || permissions.is_some_and(|p| p.can_execute());
        Ok(Attributes {
            directory,
            size: metadata.len,
            mode: u32::from(readable) * 0o444
                | u32::from(writable) * 0o222
                | u32::from(executable) * 0o111,
        })
    }
    fn dispatch(&mut self, request: Request) -> VfsResult<Response> {
        match request {
            Request::Stat { path } => Ok(Response::Attributes(
                self.attributes(&VfsPathBuf::from_units(path)?)?,
            )),
            Request::List { path } => {
                let path = VfsPathBuf::from_units(path)?;
                let mut entries = Vec::new();
                for entry in self.filesystem.read_dir(&path)? {
                    let child = path.as_path().join(&entry.name);
                    entries.push(Entry {
                        name: entry.name.as_slice().to_vec(),
                        attributes: self.attributes(&child)?,
                    });
                }
                Ok(Response::Entries(entries))
            }
            Request::Open { path, read, write } => {
                let file = self.filesystem.open(
                    &VfsPathBuf::from_units(path)?,
                    OpenOptions {
                        read,
                        write,
                        truncate: false,
                    },
                )?;
                let id = self.next;
                self.next += 1;
                self.files.insert(id, file);
                Ok(Response::Handle(id))
            }
            Request::Read {
                handle,
                offset,
                length,
            } => {
                let mut data = vec![0; length.min(MAX_IO)];
                let count = self
                    .files
                    .get(&handle)
                    .ok_or(VfsError::StaleNode)?
                    .read(offset, &mut data)?;
                data.truncate(count);
                Ok(Response::Data(data))
            }
            Request::Write {
                handle,
                offset,
                data,
            } => {
                if data.len() > MAX_IO {
                    return Err(VfsError::Io(io::Error::from_raw_os_error(libc::E2BIG)));
                }
                Ok(Response::Count(
                    self.files
                        .get(&handle)
                        .ok_or(VfsError::StaleNode)?
                        .write(offset, &data)?,
                ))
            }
            Request::Close { handle } => {
                if let Some(file) = self.files.remove(&handle) {
                    file.flush()?;
                }
                Ok(Response::Done)
            }
            other => self.mutate(other),
        }
    }
    fn mutate(&mut self, request: Request) -> VfsResult<Response> {
        match request {
            Request::Create { path, directory } => {
                let path = VfsPathBuf::from_units(path)?;
                if directory {
                    self.filesystem.create_dir(&path)?;
                } else {
                    self.filesystem
                        .create_file(&path, CreateOptions { exclusive: true })?;
                }
            }
            Request::Remove { path, directory } => self.filesystem.remove(
                &VfsPathBuf::from_units(path)?,
                if directory {
                    RemoveKind::Directory
                } else {
                    RemoveKind::File
                },
            )?,
            Request::Rename {
                source,
                target,
                replace,
            } => self.filesystem.rename_with_options(
                &VfsPathBuf::from_units(source)?,
                &VfsPathBuf::from_units(target)?,
                replace,
            )?,
            Request::SetAttributes {
                path,
                size,
                readonly,
            } => self.filesystem.set_metadata(
                &VfsPathBuf::from_units(path)?,
                MetadataChanges {
                    len: size,
                    readonly,
                },
            )?,
            Request::Sync => {
                for file in self.files.values() {
                    file.sync()?;
                }
            }
            _ => unreachable!("read-only request handled in dispatch"),
        }
        Ok(Response::Done)
    }
}
