use bcast::{HEADER_SIZE, Reader, RingBuffer, Writer};
use memmap2::{Mmap, MmapMut, MmapOptions};
use std::{
    fs::File,
    io,
    os::windows::io::{AsRawHandle, FromRawHandle},
    sync::atomic::{AtomicU64, Ordering},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE},
    System::Threading::{GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE},
};

const CAPACITY: usize = 1 << 20;
static NEXT_MAP: AtomicU64 = AtomicU64::new(1);

pub struct Publisher {
    writer: Writer,
    _mapping: MmapMut,
    file: tempfile::NamedTempFile,
}

// bcast endpoints contain raw pointers into the mapping and are intentionally
// thread-affine. Moving ownership to a dedicated IPC thread preserves that
// invariant; no endpoint is shared concurrently.
unsafe impl Send for Publisher {}
// Access to the bcast writer is exposed only through `&mut Publisher`; shared
// references are used solely for immutable handle negotiation.
unsafe impl Sync for Publisher {}

impl Publisher {
    pub fn create(label: &str) -> io::Result<Self> {
        let file = tempfile::Builder::new()
            .prefix(&format!(
                "snowglass-{label}-{}-{}-",
                std::process::id(),
                NEXT_MAP.fetch_add(1, Ordering::Relaxed)
            ))
            .suffix(".map")
            .tempfile()?;
        file.as_file().set_len((HEADER_SIZE + CAPACITY) as u64)?;
        // SAFETY: the file remains owned by this endpoint and is not resized.
        let mapping = unsafe { MmapOptions::new().map_mut(file.as_file())? };
        let writer = RingBuffer::new(&mapping).into_writer();
        Ok(Self {
            writer,
            _mapping: mapping,
            file,
        })
    }

    pub fn duplicate_for(&self, process_id: u32) -> io::Result<i64> {
        // SAFETY: OpenProcess is called with a concrete peer PID obtained from
        // the control handshake.
        let process = unsafe { OpenProcess(PROCESS_DUP_HANDLE, 0, process_id) };
        if process.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut remote = std::ptr::null_mut();
        // SAFETY: source is a live file handle and `remote` is writable. The
        // resulting handle is owned by the peer process.
        let ok = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                self.file.as_file().as_raw_handle() as HANDLE,
                process,
                &raw mut remote,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        // SAFETY: this process owns the process handle returned above.
        unsafe { CloseHandle(process) };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(remote as isize as i64)
        }
    }

    pub fn publish(&mut self, bytes: &[u8]) -> io::Result<()> {
        if bytes.len() > self.writer.mtu() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "shared IPC message exceeds MTU",
            ));
        }
        let mut claim = self.writer.claim(bytes.len(), true);
        claim.get_buffer_mut().copy_from_slice(bytes);
        claim.commit();
        Ok(())
    }

    pub const fn mapping_len(&self) -> u64 {
        (HEADER_SIZE + CAPACITY) as u64
    }
}

pub struct Subscriber {
    reader: Reader,
    _mapping: Mmap,
    _file: File,
}

unsafe impl Send for Subscriber {}

impl Subscriber {
    pub unsafe fn from_handle(handle: i64, len: u64) -> io::Result<Self> {
        if len != (HEADER_SIZE + CAPACITY) as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected shared IPC mapping size",
            ));
        }
        // SAFETY: ownership of the duplicated file handle was transferred by
        // the control message to this process.
        let file = unsafe { File::from_raw_handle(handle as isize as _) };
        // SAFETY: the negotiated file has the validated fixed size and remains
        // owned for the lifetime of the mapping.
        let mapping = unsafe { MmapOptions::new().len(len as usize).map(&file)? };
        let reader = RingBuffer::new(&mapping).into_reader();
        Ok(Self {
            reader,
            _mapping: mapping,
            _file: file,
        })
    }

    pub fn latest(&mut self, destination: &mut Vec<u8>) -> io::Result<bool> {
        let Some(batch) = self.reader.read_batch() else {
            return Ok(false);
        };
        let mut found = false;
        for message in batch.into_iter() {
            match message {
                Ok(message) => {
                    destination.resize(message.payload_len, 0);
                    message
                        .read(destination)
                        .map_err(|error| io::Error::other(error.to_string()))?;
                    found = true;
                }
                Err(_) => {
                    self.reader.reset();
                    return Ok(false);
                }
            }
        }
        Ok(found)
    }

    pub fn drain(
        &mut self,
        destination: &mut Vec<u8>,
        mut receive: impl FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<usize> {
        let Some(batch) = self.reader.read_batch() else {
            return Ok(0);
        };
        let mut count = 0;
        for message in batch.into_iter() {
            match message {
                Ok(message) => {
                    destination.resize(message.payload_len, 0);
                    message
                        .read(destination)
                        .map_err(|error| io::Error::other(error.to_string()))?;
                    receive(destination)?;
                    count += 1;
                }
                Err(_) => {
                    self.reader.reset();
                    break;
                }
            }
        }
        Ok(count)
    }
}
