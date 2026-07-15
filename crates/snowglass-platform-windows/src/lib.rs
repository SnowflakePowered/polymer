//! Windows-specific handle, adapter, process, and window utilities.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct ProcessId(pub u32);

#[cfg(windows)]
pub fn performance_counter() -> std::io::Result<u64> {
    let mut value = 0i64;
    // SAFETY: the pointer refers to writable storage for the duration of the call.
    if unsafe { windows_sys::Win32::System::Performance::QueryPerformanceCounter(&mut value) } == 0
    {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(value as u64)
    }
}

#[cfg(windows)]
pub fn performance_frequency() -> std::io::Result<u64> {
    let mut value = 0i64;
    // SAFETY: the pointer refers to writable storage for the duration of the call.
    if unsafe { windows_sys::Win32::System::Performance::QueryPerformanceFrequency(&mut value) }
        == 0
    {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(value as u64)
    }
}

#[cfg(windows)]
pub fn duplicate_handle_into_process(
    source_handle: isize,
    target_process_id: ProcessId,
) -> std::io::Result<isize> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE},
        System::Threading::{GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE},
    };

    // SAFETY: handles are checked for null, the target process handle is closed
    // on every path, and DuplicateHandle initializes `duplicated` on success.
    unsafe {
        let target_process = OpenProcess(PROCESS_DUP_HANDLE, 0, target_process_id.0);
        if target_process.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let mut duplicated: HANDLE = std::ptr::null_mut();
        let result = DuplicateHandle(
            GetCurrentProcess(),
            source_handle as HANDLE,
            target_process,
            &mut duplicated,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        );
        let error = (result == 0).then(std::io::Error::last_os_error);
        CloseHandle(target_process);
        if let Some(error) = error {
            Err(error)
        } else {
            Ok(duplicated as isize)
        }
    }
}

#[cfg(windows)]
pub fn hide_top_level_windows(process_id: ProcessId) -> std::io::Result<u32> {
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM},
        UI::WindowsAndMessaging::{EnumWindows, GetWindowThreadProcessId, SW_HIDE, ShowWindow},
    };

    struct Context {
        process_id: u32,
        hidden: u32,
    }

    unsafe extern "system" fn callback(window: HWND, parameter: LPARAM) -> i32 {
        // SAFETY: EnumWindows supplies the pointer passed below for the complete
        // duration of enumeration.
        let context = unsafe { &mut *(parameter as *mut Context) };
        let mut owner = 0;
        unsafe { GetWindowThreadProcessId(window, &mut owner) };
        if owner == context.process_id {
            unsafe { ShowWindow(window, SW_HIDE) };
            context.hidden += 1;
        }
        1
    }

    let mut context = Context {
        process_id: process_id.0,
        hidden: 0,
    };
    // SAFETY: callback and context remain valid until EnumWindows returns.
    if unsafe { EnumWindows(Some(callback), (&mut context as *mut Context) as LPARAM) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(context.hidden)
}
