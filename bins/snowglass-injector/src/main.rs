#[cfg(not(windows))]
compile_error!("snowglass-injector is only supported on Windows");

use clap::Parser;
use std::{
    ffi::{CStr, CString},
    mem,
    os::windows::{ffi::OsStrExt, io::FromRawHandle},
    path::{Path, PathBuf},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, FreeLibrary, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0},
    System::{
        Diagnostics::{
            Debug::WriteProcessMemory,
            ToolHelp::{
                CreateToolhelp32Snapshot, MODULEENTRY32W, Module32FirstW, Module32NextW,
                TH32CS_SNAPMODULE, TH32CS_SNAPMODULE32,
            },
        },
        LibraryLoader::{
            DONT_RESOLVE_DLL_REFERENCES, GetModuleHandleW, GetProcAddress, LoadLibraryExW,
        },
        Memory::{
            MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAllocEx, VirtualFreeEx,
        },
        Threading::{
            CreateRemoteThread, GetExitCodeThread, INFINITE, OpenProcess, PROCESS_CREATE_THREAD,
            PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ, PROCESS_VM_WRITE,
            WaitForSingleObject,
        },
    },
};

struct Library {
    path: PathBuf,
    export: CString,
}

#[derive(Debug, Parser)]
#[command(name = "snowglass-injector", version, about)]
struct Options {
    /// Process ID of the suspended target.
    #[arg(long)]
    pid: u32,

    /// DLL to inject. May be repeated.
    #[arg(long, required = true)]
    library: Vec<PathBuf>,

    /// Installer export corresponding to each --library.
    #[arg(long, required = true)]
    export: Vec<String>,
}

fn main() {
    if let Err(error) = run(Options::parse()) {
        eprintln!("snowglass-injector: {error}");
        std::process::exit(1);
    }
}

fn run(options: Options) -> Result<(), Box<dyn std::error::Error>> {
    let libraries = options.libraries()?;
    let process = unsafe {
        OpenProcess(
            PROCESS_CREATE_THREAD
                | PROCESS_QUERY_INFORMATION
                | PROCESS_VM_OPERATION
                | PROCESS_VM_READ
                | PROCESS_VM_WRITE,
            0,
            options.pid,
        )
    };
    if process.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    let process = unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(process) };
    use std::os::windows::io::AsRawHandle;
    for library in libraries {
        inject_library(
            process.as_raw_handle(),
            options.pid,
            &library.path,
            &library.export,
        )?;
    }
    Ok(())
}

impl Options {
    fn libraries(&self) -> Result<Vec<Library>, Box<dyn std::error::Error>> {
        if self.library.len() != self.export.len() {
            return Err(format!(
                "received {} --library arguments but {} --export arguments",
                self.library.len(),
                self.export.len()
            )
            .into());
        }
        self.library
            .iter()
            .cloned()
            .zip(&self.export)
            .map(|(path, export)| {
                Ok(Library {
                    path,
                    export: CString::new(export.as_bytes())?,
                })
            })
            .collect()
    }
}

fn inject_library(
    process: HANDLE,
    process_id: u32,
    library: &Path,
    install_export: &CStr,
) -> std::io::Result<()> {
    let mut wide = library
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let bytes = wide.len() * mem::size_of::<u16>();
    let remote_path = unsafe {
        VirtualAllocEx(
            process,
            std::ptr::null(),
            bytes,
            MEM_COMMIT | MEM_RESERVE,
            PAGE_READWRITE,
        )
    };
    if remote_path.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    let result = (|| {
        let mut written = 0;
        if unsafe {
            WriteProcessMemory(
                process,
                remote_path,
                wide.as_mut_ptr().cast(),
                bytes,
                &mut written,
            )
        } == 0
            || written != bytes
        {
            return Err(std::io::Error::last_os_error());
        }
        let kernel_name = "kernel32.dll\0".encode_utf16().collect::<Vec<_>>();
        let kernel = unsafe { GetModuleHandleW(kernel_name.as_ptr()) };
        if kernel.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let load_library = unsafe { GetProcAddress(kernel, c"LoadLibraryW".as_ptr().cast()) }
            .ok_or_else(std::io::Error::last_os_error)?;
        let load_thread = unsafe {
            CreateRemoteThread(
                process,
                std::ptr::null(),
                0,
                Some(std::mem::transmute(load_library)),
                remote_path,
                0,
                std::ptr::null_mut(),
            )
        };
        if load_thread.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        // The thread exit value is only a DWORD and cannot reliably carry a
        // 64-bit HMODULE. Confirm the load through the target's module list.
        wait_remote_thread(load_thread)?;

        let remote_module = find_remote_module(process_id, library)?;
        let local_module = unsafe {
            LoadLibraryExW(
                wide.as_ptr(),
                std::ptr::null_mut(),
                DONT_RESOLVE_DLL_REFERENCES,
            )
        };
        if local_module.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let local_export = unsafe { GetProcAddress(local_module, install_export.as_ptr().cast()) }
            .ok_or_else(std::io::Error::last_os_error)?;
        let export_offset = local_export as usize - local_module as usize;
        unsafe { FreeLibrary(local_module) };
        let remote_export = (remote_module + export_offset) as *const ();
        let install_thread = unsafe {
            CreateRemoteThread(
                process,
                std::ptr::null(),
                0,
                Some(std::mem::transmute(remote_export)),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
            )
        };
        if install_thread.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let install_result = wait_remote_thread(install_thread)?;
        if install_result != 0 {
            return Err(std::io::Error::other(format!(
                "remote hook installer returned HRESULT {install_result:#x}"
            )));
        }
        Ok(())
    })();
    unsafe { VirtualFreeEx(process, remote_path, 0, MEM_RELEASE) };
    result
}

fn wait_remote_thread(thread: HANDLE) -> std::io::Result<u32> {
    let wait = unsafe { WaitForSingleObject(thread, INFINITE) };
    if wait != WAIT_OBJECT_0 {
        let error = std::io::Error::last_os_error();
        unsafe { CloseHandle(thread) };
        return Err(error);
    }
    let mut exit_code = 0;
    let ok = unsafe { GetExitCodeThread(thread, &mut exit_code) };
    unsafe { CloseHandle(thread) };
    if ok == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(exit_code)
    }
}

fn find_remote_module(process_id: u32, library: &Path) -> std::io::Result<usize> {
    let expected = library
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid DLL name"))?;
    let mut retries = 0;
    let snapshot = loop {
        let snapshot = unsafe {
            CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, process_id)
        };
        if snapshot != INVALID_HANDLE_VALUE {
            break snapshot;
        }
        let error = std::io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(24 | 299)) {
            return Err(error);
        }
        retries += 1;
        if retries == 250 {
            return Err(error);
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    };
    let mut entry = MODULEENTRY32W {
        dwSize: mem::size_of::<MODULEENTRY32W>() as u32,
        ..Default::default()
    };
    let mut found = None;
    if unsafe { Module32FirstW(snapshot, &mut entry) } != 0 {
        loop {
            let length = entry
                .szModule
                .iter()
                .position(|unit| *unit == 0)
                .unwrap_or(entry.szModule.len());
            if String::from_utf16_lossy(&entry.szModule[..length]).eq_ignore_ascii_case(expected) {
                found = Some(entry.modBaseAddr as usize);
                break;
            }
            if unsafe { Module32NextW(snapshot, &mut entry) } == 0 {
                break;
            }
        }
    }
    unsafe { CloseHandle(snapshot) };
    found.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "injected DLL was not present in target module list",
        )
    })
}
