use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

pub fn performance_counter() -> std::io::Result<u64> {
    let mut value = 0;
    unsafe { QueryPerformanceCounter(&mut value) }.map_err(|_| std::io::Error::last_os_error())?;
    Ok(value as u64)
}

pub fn performance_frequency() -> std::io::Result<u64> {
    let mut value = 0;
    unsafe { QueryPerformanceFrequency(&mut value) }
        .map_err(|_| std::io::Error::last_os_error())?;
    Ok(value as u64)
}
