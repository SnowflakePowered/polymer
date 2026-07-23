use crate::ProcessId;
use windows::Win32::{
    Foundation::{CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE},
    System::Threading::{GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE},
};

pub fn duplicate_handle_into_process(
    source_handle: isize,
    target_process_id: ProcessId,
) -> std::io::Result<isize> {
    unsafe {
        let target = OpenProcess(PROCESS_DUP_HANDLE, false, target_process_id.0)
            .map_err(|_| std::io::Error::last_os_error())?;
        let mut duplicated = HANDLE::default();
        let result = DuplicateHandle(
            GetCurrentProcess(),
            HANDLE(source_handle as *mut _),
            target,
            &mut duplicated,
            0,
            false,
            DUPLICATE_SAME_ACCESS,
        );
        let error = result.err().map(|_| std::io::Error::last_os_error());
        let _ = CloseHandle(target);
        match error {
            Some(error) => Err(error),
            None => Ok(duplicated.0 as isize),
        }
    }
}
