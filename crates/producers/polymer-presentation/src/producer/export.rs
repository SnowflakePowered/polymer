use polymer_ipc::StreamReady;
use polymer_protocol::MAX_IMAGE_SLOTS;
use std::io;
use windows::Win32::{
    Foundation::{
        CloseHandle, DUPLICATE_CLOSE_SOURCE, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE,
    },
    System::Threading::{GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE},
};

/// Owns remote duplicates until publication transfers them to the receiver.
pub(super) struct Export {
    process: HANDLE,
    handles: Vec<HANDLE>,
    published: bool,
}

impl Export {
    pub(super) fn prepare(pid: u32, ready: &mut StreamReady) -> io::Result<Self> {
        if ready.slot_count == 0 || ready.slot_count as usize > MAX_IMAGE_SLOTS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid capture slot count",
            ));
        }
        let process =
            unsafe { OpenProcess(PROCESS_DUP_HANDLE, false, pid) }.map_err(io::Error::other)?;
        let mut export = Self {
            process,
            handles: Vec::new(),
            published: false,
        };
        for handle in &mut ready.image_handles[..ready.slot_count as usize] {
            *handle = export.duplicate(*handle)?;
        }
        for handle in [
            &mut ready.timeline_semaphore_handle,
            &mut ready.consumer_semaphore_handle,
        ] {
            if *handle != 0 {
                *handle = export.duplicate(*handle)?;
            }
        }
        Ok(export)
    }

    fn duplicate(&mut self, raw: i64) -> io::Result<i64> {
        let mut duplicated = HANDLE::default();
        unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                HANDLE(raw as *mut _),
                self.process,
                &mut duplicated,
                0,
                false,
                DUPLICATE_SAME_ACCESS,
            )
        }
        .map_err(io::Error::other)?;
        self.handles.push(duplicated);
        Ok(duplicated.0 as i64)
    }

    pub(super) fn transfer(&mut self) {
        self.published = true;
    }
}

impl Drop for Export {
    fn drop(&mut self) {
        unsafe {
            if !self.published {
                for &handle in &self.handles {
                    let mut local = HANDLE::default();
                    if DuplicateHandle(
                        self.process,
                        handle,
                        GetCurrentProcess(),
                        &mut local,
                        0,
                        false,
                        DUPLICATE_CLOSE_SOURCE | DUPLICATE_SAME_ACCESS,
                    )
                    .is_ok()
                    {
                        let _ = CloseHandle(local);
                    }
                }
            }
            let _ = CloseHandle(self.process);
        }
    }
}
