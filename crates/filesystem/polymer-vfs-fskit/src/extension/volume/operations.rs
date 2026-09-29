use super::*;

impl Volume {
    // FSKit may synchronously query the volume from a reply block. Release the
    // namespace lock before replying so those callbacks can reenter safely.
    pub(super) fn with_namespace<T>(&self, operation: impl FnOnce() -> T) -> T {
        let _guard = self.ivars().namespace.lock();
        operation()
    }

    pub fn new(resource: &Resource, readonly: bool) -> io::Result<Retained<Self>> {
        let connection = Connection::connect(resource)?;
        let this = Self::alloc().set_ivars(State {
            connection: Mutex::new(connection),
            items: DashMap::new(),
            next: AtomicU64::new(3),
            namespace: Mutex::new(()),
            readonly,
        });
        let name =
            unsafe { FSFileName::nameWithData(&NSData::with_bytes(resource.name.as_bytes())) };
        Ok(unsafe {
            msg_send![super(this), initWithVolumeID: &*resource.identifier(), volumeName: &*name]
        })
    }
    pub(super) fn call(&self, request: Request) -> io::Result<Response> {
        self.ivars().connection.lock().call(&request)
    }
    pub(super) fn checked(item: &FSItem) -> io::Result<&Item> {
        item.downcast_ref()
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))
    }
    pub(super) fn writable(&self) -> io::Result<()> {
        if self.ivars().readonly {
            Err(io::Error::from_raw_os_error(libc::EROFS))
        } else {
            Ok(())
        }
    }
    pub(super) fn stat(&self, item: &FSItem) -> io::Result<Retained<FSItemAttributes>> {
        let item = Self::checked(item)?;
        let Response::Attributes(attributes) = self.call(Request::Stat { path: item.path() })?
        else {
            return Err(Error::protocol());
        };
        Ok(item.attributes(&attributes))
    }
    pub(super) fn child_path(directory: &FSItem, name: &FSFileName) -> io::Result<Vec<u8>> {
        let name = unsafe { name.data().to_vec() };
        if name.is_empty()
            || name == b"."
            || name == b".."
            || name.contains(&b'/')
            || name.contains(&0)
        {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        let mut path = Self::checked(directory)?.path();
        if path != b"/" {
            path.push(b'/');
        }
        path.extend(name);
        Ok(path)
    }
    pub(super) fn item(&self, path: Vec<u8>) -> io::Result<Retained<Item>> {
        // Validate before publishing the item, even if its ID is already cached.
        self.call(Request::Stat { path: path.clone() })?;
        Ok(self
            .ivars()
            .items
            .entry(path.clone())
            .or_insert_with(|| {
                let id = if path == b"/" {
                    2
                } else {
                    self.ivars().next.fetch_add(1, Ordering::Relaxed)
                };
                Item::new(id, path)
            })
            .clone())
    }
    pub(super) fn create(
        &self,
        directory: &FSItem,
        name: &FSFileName,
        kind: FSItemType,
        attrs: &FSItemSetAttributesRequest,
    ) -> io::Result<Retained<Item>> {
        self.writable()?;
        if kind != FSItemType::File && kind != FSItemType::Directory {
            return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
        }
        let path = Self::child_path(directory, name)?;
        self.call(Request::Create {
            path: path.clone(),
            directory: kind == FSItemType::Directory,
        })?;
        let item = self.item(path)?;
        self.set_attributes(&item, attrs)?;
        Ok(item)
    }
    pub(super) fn set_attributes(
        &self,
        item: &FSItem,
        attrs: &FSItemSetAttributesRequest,
    ) -> io::Result<Retained<FSItemAttributes>> {
        self.writable()?;
        let directory = unsafe { self.stat(item)?.r#type() == FSItemType::Directory };
        let mut consumed = FSItemAttribute::empty();
        let size = unsafe {
            (attrs.isValid(FSItemAttribute::Size) && !directory).then(|| {
                consumed |= FSItemAttribute::Size;
                attrs.size()
            })
        };
        let readonly = unsafe {
            attrs.isValid(FSItemAttribute::Mode).then(|| {
                consumed |= FSItemAttribute::Mode;
                attrs.mode() & 0o222 == 0
            })
        };
        if size.is_some() || readonly.is_some() {
            self.call(Request::SetAttributes {
                path: Self::checked(item)?.path(),
                size,
                readonly,
            })?;
        }
        unsafe { attrs.setConsumedAttributes(consumed) };
        self.stat(item)
    }
    pub(super) fn open(&self, item: &FSItem, modes: FSVolumeOpenModes) -> io::Result<()> {
        if unsafe { self.stat(item)?.r#type() == FSItemType::Directory } {
            return Ok(());
        }
        let item = Self::checked(item)?;
        let read = modes.contains(FSVolumeOpenModes::Read);
        let write = modes.contains(FSVolumeOpenModes::Write);
        if write {
            self.writable()?;
        }
        let mut current = item.ivars().handle.lock();
        if current.is_some_and(|(_, r, w)| (!read || r) && (!write || w)) {
            return Ok(());
        }
        let read = read || current.is_some_and(|(_, read, _)| read);
        let write = write || current.is_some_and(|(_, _, write)| write);
        let Response::Handle(handle) = self.call(Request::Open {
            path: item.path(),
            read,
            write,
        })?
        else {
            return Err(Error::protocol());
        };
        if let Some((old, _, _)) = current.replace((handle, read, write)) {
            self.call(Request::Close { handle: old })?;
        }
        Ok(())
    }
    pub(super) fn close(&self, item: &FSItem) -> io::Result<()> {
        if let Some((handle, _, _)) = Self::checked(item)?.ivars().handle.lock().take() {
            self.call(Request::Close { handle })?;
        }
        Ok(())
    }
    pub(super) fn read(
        &self,
        item: &FSItem,
        offset: i64,
        length: usize,
        buffer: &FSMutableFileDataBuffer,
    ) -> io::Result<usize> {
        let offset =
            u64::try_from(offset).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
        // Executable page-ins can arrive without a preceding open callback.
        // Acquire a VFS read handle for these kernel-authorized reads as well.
        self.open(item, FSVolumeOpenModes::Read)?;
        let handle = Self::checked(item)?
            .ivars()
            .handle
            .lock()
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF))?
            .0;
        let length = length.min(MAX_IO).min(unsafe { buffer.length() });
        let Response::Data(data) = self.call(Request::Read {
            handle,
            offset,
            length,
        })?
        else {
            return Err(Error::protocol());
        };
        if data.len() > length {
            return Err(Error::protocol());
        }
        unsafe {
            ptr::copy_nonoverlapping(
                data.as_ptr(),
                buffer.mutableBytes().as_ptr().cast(),
                data.len(),
            )
        };
        Ok(data.len())
    }
    pub(super) fn write(&self, item: &FSItem, offset: i64, data: &NSData) -> io::Result<usize> {
        self.writable()?;
        let offset =
            u64::try_from(offset).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
        let handle = Self::checked(item)?
            .ivars()
            .handle
            .lock()
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF))?
            .0;
        let mut data = data.to_vec();
        data.truncate(MAX_IO);
        let Response::Count(count) = self.call(Request::Write {
            handle,
            offset,
            data,
        })?
        else {
            return Err(Error::protocol());
        };
        Ok(count)
    }
    pub(super) fn remove(
        &self,
        item: &FSItem,
        directory: &FSItem,
        name: &FSFileName,
    ) -> io::Result<()> {
        self.writable()?;
        let path = Self::child_path(directory, name)?;
        let Response::Attributes(attrs) = self.call(Request::Stat { path: path.clone() })? else {
            return Err(Error::protocol());
        };
        if path != Self::checked(item)?.path() {
            return Err(io::Error::from_raw_os_error(libc::ESTALE));
        }
        self.call(Request::Remove {
            path: path.clone(),
            directory: attrs.directory,
        })?;
        self.ivars().items.remove(&path);
        Ok(())
    }
    pub(super) fn rename(
        &self,
        item: &FSItem,
        directory: &FSItem,
        name: &FSFileName,
        replace: bool,
    ) -> io::Result<()> {
        self.writable()?;
        let source = Self::checked(item)?.path();
        let target = Self::child_path(directory, name)?;
        if source == target {
            return Ok(());
        }
        self.call(Request::Rename {
            source: source.clone(),
            target: target.clone(),
            replace,
        })?;
        self.ivars().items.remove(&target);
        let changed: Vec<_> = self
            .ivars()
            .items
            .iter()
            .filter(|entry| {
                entry.key() == &source
                    || (entry.key().starts_with(&source)
                        && entry.key().get(source.len()) == Some(&b'/'))
            })
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect();
        for (old, item) in changed {
            let mut path = target.clone();
            path.extend_from_slice(&old[source.len()..]);
            self.ivars().items.remove(&old);
            *item.ivars().path.write() = path.clone();
            self.ivars().items.insert(path, item);
        }
        Ok(())
    }
    pub(super) fn enumerate(
        &self,
        directory: &FSItem,
        cookie: u64,
        verifier: u64,
        attributes: bool,
        packer: &FSDirectoryEntryPacker,
    ) -> io::Result<u64> {
        let item = Self::checked(directory)?;
        let mut snapshot = item.ivars().entries.lock();
        if cookie == 0 {
            let Response::Entries(mut listed) = self.call(Request::List { path: item.path() })?
            else {
                return Err(Error::protocol());
            };
            if !attributes {
                let attrs = Attributes {
                    directory: true,
                    size: 0,
                    mode: 0o555,
                };
                listed.insert(
                    0,
                    Entry {
                        name: b"..".to_vec(),
                        attributes: attrs.clone(),
                    },
                );
                listed.insert(
                    0,
                    Entry {
                        name: b".".to_vec(),
                        attributes: attrs,
                    },
                );
            }
            snapshot.verifier = snapshot.verifier.wrapping_add(1).max(1);
            snapshot.attributes = attributes;
            snapshot.entries = listed;
        } else if verifier != snapshot.verifier || attributes != snapshot.attributes {
            // A concurrent scan replaced this snapshot. Ask FSKit to restart
            // instead of using a cookie against another enumeration's entries.
            return Err(io::Error::from_raw_os_error(
                FSErrorCode::InvalidDirectoryCookie.0 as i32,
            ));
        }
        let entries = &snapshot.entries;
        if cookie as usize > entries.len() {
            return Err(io::Error::from_raw_os_error(
                FSErrorCode::InvalidDirectoryCookie.0 as i32,
            ));
        }
        for (index, entry) in entries.iter().enumerate().skip(cookie as usize) {
            let name = unsafe { FSFileName::nameWithData(&NSData::with_bytes(&entry.name)) };
            let child = if entry.name == b"." {
                self.item(item.path())?
            } else if entry.name == b".." {
                let path: polymer_vfs::VfsPathBuf =
                    polymer_vfs::VfsPathBuf::from_units(item.path()).map_err(io::Error::other)?;
                self.item(
                    path.parent()
                        .unwrap_or_else(polymer_vfs::VfsPathBuf::root)
                        .as_slice()
                        .to_vec(),
                )?
            } else {
                self.item(Self::child_path(directory, &name)?)?
            };
            let attrs = child.attributes(&entry.attributes);
            let packed = unsafe {
                packer.packEntryWithName_itemType_itemID_nextCookie_attributes(
                    &name,
                    if entry.attributes.directory {
                        FSItemType::Directory
                    } else {
                        FSItemType::File
                    },
                    FSItemID(child.ivars().id),
                    (index + 1) as u64,
                    attributes.then_some(&*attrs),
                )
            };
            if !packed {
                break;
            }
        }
        Ok(snapshot.verifier)
    }
}
