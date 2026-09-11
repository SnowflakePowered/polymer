use crate::protocol::Header;
use std::{
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    sync::atomic::{AtomicBool, Ordering},
};
use windows::{
    Win32::{
        Foundation::{HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::{
            Memory::*,
            SystemInformation::{GetSystemInfo, SYSTEM_INFO},
            Threading::*,
        },
    },
    core::w,
};

// A late reply has no transaction ID. After a timeout don't overwrite an
// outstanding command by retrying from this process.
static TIMED_OUT: AtomicBool = AtomicBool::new(false);

pub(crate) struct Connection {
    _mapping: OwnedHandle,
    mutex: OwnedHandle,
    read: OwnedHandle,
    write: OwnedHandle,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    page_size: usize,
}

impl Drop for Connection {
    fn drop(&mut self) {
        // SAFETY: this view is uniquely owned and all operations have completed.
        unsafe {
            let _ = UnmapViewOfFile(self.view);
        }
    }
}

pub(crate) struct CommandGuard<'a>(&'a OwnedHandle);
impl Drop for CommandGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: constructed only after this thread acquired the named mutex.
        unsafe {
            let _ = ReleaseMutex(HANDLE(self.0.as_raw_handle()));
        }
    }
}

impl Connection {
    pub fn open() -> io::Result<Self> {
        // SAFETY: Open* return owned handles; no object is created or elevated.
        let (mapping, mutex, read, write) = unsafe {
            let mapping = OpenFileMappingW(
                (FILE_MAP_READ | FILE_MAP_WRITE).0,
                false,
                w!("Global\\DsHidMiniSharedMemory"),
            )
            .map_err(|error| {
                io::Error::other(format!(
                    "DsHidMini IPC unavailable: {error}; enable IPC and reconnect the controller"
                ))
            })?;
            let mapping = OwnedHandle::from_raw_handle(mapping.0);
            let mutex = OpenMutexW(
                SYNCHRONIZATION_SYNCHRONIZE | MUTEX_MODIFY_STATE,
                false,
                w!("Global\\DsHidMiniCommandMutex"),
            )
            .map_err(io::Error::other)?;
            let mutex = OwnedHandle::from_raw_handle(mutex.0);
            let read = OpenEventW(EVENT_MODIFY_STATE, false, w!("Global\\DsHidMiniReadEvent"))
                .map_err(io::Error::other)?;
            let read = OwnedHandle::from_raw_handle(read.0);
            let write = OpenEventW(
                SYNCHRONIZATION_SYNCHRONIZE | EVENT_MODIFY_STATE,
                false,
                w!("Global\\DsHidMiniWriteEvent"),
            )
            .map_err(io::Error::other)?;
            (mapping, mutex, read, OwnedHandle::from_raw_handle(write.0))
        };
        let mut info = SYSTEM_INFO::default();
        // SAFETY: initialized output and valid mapping handle; two IPC regions.
        let view = unsafe {
            GetSystemInfo(&mut info);
            MapViewOfFile(
                HANDLE(mapping.as_raw_handle()),
                FILE_MAP_READ | FILE_MAP_WRITE,
                0,
                0,
                info.dwAllocationGranularity as usize * 2,
            )
        };
        if view.Value.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            _mapping: mapping,
            mutex,
            read,
            write,
            view,
            page_size: info.dwAllocationGranularity as usize,
        })
    }

    pub fn lock(&self) -> io::Result<CommandGuard<'_>> {
        // SAFETY: live mutex; bounded wait on this calling thread.
        match unsafe { WaitForSingleObject(HANDLE(self.mutex.as_raw_handle()), 2000) } {
            WAIT_OBJECT_0 => Ok(CommandGuard(&self.mutex)),
            WAIT_ABANDONED => {
                let _guard = CommandGuard(&self.mutex);
                TIMED_OUT.store(true, Ordering::Release);
                Err(io::Error::other(
                    "DsHidMini command mutex was abandoned; reconnect the controller and restart Polymer",
                ))
            }
            WAIT_TIMEOUT => Err(io::Error::other(
                "DsHidMini IPC is busy; close other configuration tools and retry",
            )),
            _ => Err(io::Error::last_os_error()),
        }
    }

    pub fn legacy_slot(&self) -> io::Result<u32> {
        let mut records = vec![0; 53 * 255];
        // SAFETY: the mapped second region is at least 64KiB, greater than
        // these 255 packed records. Volatile copies avoid borrowing driver memory.
        for (index, byte) in records.iter_mut().enumerate() {
            *byte = unsafe {
                self.view
                    .Value
                    .cast::<u8>()
                    .add(self.page_size + index)
                    .read_volatile()
            };
        }
        Header::legacy_slot(&records)
    }

    pub fn pair(&self, slot: u32, host: [u8; 6]) -> io::Result<()> {
        if TIMED_OUT.load(Ordering::Acquire) {
            return Err(io::Error::other(
                "previous DsHidMini command did not finish; reconnect the controller and restart Polymer before retrying",
            ));
        }
        let request = Header::pair_request(slot, host);
        // SAFETY: caller holds the driver-wide command mutex on this thread;
        // command region is writable and larger than request/reply. Event
        // signaling hands ownership to the driver until the reply event fires.
        unsafe {
            ResetEvent(HANDLE(self.write.as_raw_handle())).map_err(io::Error::other)?;
            for (index, byte) in request.iter().enumerate() {
                self.view
                    .Value
                    .cast::<u8>()
                    .add(index)
                    .write_volatile(*byte);
            }
            SetEvent(HANDLE(self.read.as_raw_handle())).map_err(io::Error::other)?;
            if WaitForSingleObject(HANDLE(self.write.as_raw_handle()), 3000) != WAIT_OBJECT_0 {
                TIMED_OUT.store(true, Ordering::Release);
                return Err(io::Error::other(
                    "DsHidMini pairing reply timed out; the write may have occurred. Reconnect and read addresses before retrying",
                ));
            }
        }
        let mut reply = [0; 28];
        for (index, byte) in reply.iter_mut().enumerate() {
            // SAFETY: driver signaled completion, command mutex still held.
            *byte = unsafe { self.view.Value.cast::<u8>().add(index).read_volatile() };
        }
        Header::validate_reply(&reply, slot)
    }
}
