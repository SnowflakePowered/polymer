#[cfg(not(windows))]
compile_error!("snowglass-watcher is only supported on Windows");

use std::{
    ffi::OsString,
    io::{self, Write},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
};

mod game_bar;
mod lifecycle;
mod steam;

use lifecycle::LifecycleAdjustments;
use windows::Win32::{
    Foundation::{HANDLE, WAIT_FAILED, WAIT_OBJECT_0},
    System::Threading::{INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
};

fn main() {
    if let Err(error) = run() {
        eprintln!("snowglass-watcher: {error}");
        std::process::exit(1);
    }
}

fn run() -> io::Result<()> {
    let parent_id = parse_parent_id(std::env::args_os().skip(1))?;
    let parent = open_process(parent_id)?;
    let _adjustments = LifecycleAdjustments::new()?;

    println!("READY");
    io::stdout().flush()?;

    wait_for_process(&parent)
}

fn parse_parent_id(mut arguments: impl Iterator<Item = OsString>) -> io::Result<u32> {
    let flag = arguments.next();
    let value = arguments.next();
    if flag.as_deref() != Some(std::ffi::OsStr::new("--parent-pid"))
        || value.is_none()
        || arguments.next().is_some()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: snowglass-watcher --parent-pid <process-id>",
        ));
    }
    value
        .expect("checked above")
        .to_string_lossy()
        .parse::<u32>()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid parent process id"))
}

fn open_process(process_id: u32) -> io::Result<OwnedHandle> {
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, process_id) }
        .map_err(|error| io::Error::from_raw_os_error(error.code().0))?;
    Ok(unsafe { OwnedHandle::from_raw_handle(handle.0) })
}

fn wait_for_process(process: &OwnedHandle) -> io::Result<()> {
    let handle = HANDLE(process.as_raw_handle());
    match unsafe { WaitForSingleObject(handle, INFINITE) } {
        WAIT_OBJECT_0 => Ok(()),
        WAIT_FAILED => Err(io::Error::last_os_error()),
        result => Err(io::Error::other(format!(
            "unexpected parent wait result {result:?}"
        ))),
    }
}
