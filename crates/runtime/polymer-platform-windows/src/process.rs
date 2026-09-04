use crate::ProcessId;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessArchitecture {
    X86_64,
    Arm64,
}

impl ProcessArchitecture {
    pub const fn executable_suffix(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Arm64 => "arm64",
        }
    }
}

pub fn executable_architecture(path: &std::path::Path) -> std::io::Result<ProcessArchitecture> {
    use exe::{ImageFileMachine, NTHeaders, PE, VecPE};

    let executable = VecPE::from_disk_file(path).map_err(|error| match error {
        exe::Error::IoError(error) => error,
        error => std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()),
    })?;
    let machine = match executable
        .get_valid_nt_headers()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()))?
    {
        NTHeaders::NTHeaders32(headers) => headers.file_header.machine,
        NTHeaders::NTHeaders64(headers) => headers.file_header.machine,
    };
    match machine {
        machine if machine == ImageFileMachine::AMD64 as u16 => Ok(ProcessArchitecture::X86_64),
        machine if machine == ImageFileMachine::ARM64 as u16 => Ok(ProcessArchitecture::Arm64),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!("unsupported target machine {machine:#06x}"),
        )),
    }
}

#[cfg(windows)]
pub fn process_architecture(process: isize) -> std::io::Result<ProcessArchitecture> {
    use windows::Win32::System::Threading::{
        GetProcessInformation, PROCESS_MACHINE_INFORMATION, ProcessMachineTypeInfo,
    };

    const IMAGE_FILE_MACHINE_AMD64: u16 = 0x8664;
    const IMAGE_FILE_MACHINE_ARM64: u16 = 0xaa64;

    let mut information = PROCESS_MACHINE_INFORMATION::default();
    unsafe {
        GetProcessInformation(
            windows::Win32::Foundation::HANDLE(process as *mut _),
            ProcessMachineTypeInfo,
            (&raw mut information).cast(),
            std::mem::size_of_val(&information) as u32,
        )
    }
    .map_err(|_| std::io::Error::last_os_error())?;
    let machine = information.ProcessMachine.0;
    match machine {
        IMAGE_FILE_MACHINE_AMD64 => Ok(ProcessArchitecture::X86_64),
        IMAGE_FILE_MACHINE_ARM64 => Ok(ProcessArchitecture::Arm64),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!("unsupported target machine {machine:#06x}"),
        )),
    }
}

#[cfg(windows)]
pub fn resume_initial_thread(process_id: ProcessId) -> std::io::Result<()> {
    use windows::Win32::{
        Foundation::CloseHandle,
        System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
    };

    let initial_thread = first_process_thread(process_id)?;
    let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, false, initial_thread) }
        .map_err(|_| std::io::Error::last_os_error())?;
    let resume = unsafe { ResumeThread(thread) };
    let _ = unsafe { CloseHandle(thread) };
    if resume == u32::MAX {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
pub fn process_tree_ids(root: ProcessId) -> std::io::Result<Vec<ProcessId>> {
    use std::{collections::HashSet, mem};
    use windows::Win32::{
        Foundation::CloseHandle,
        System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS,
        },
    };

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
        .map_err(|_| std::io::Error::last_os_error())?;
    let mut entry = PROCESSENTRY32W {
        dwSize: mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut parents = Vec::new();
    if unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok() {
        loop {
            parents.push((entry.th32ProcessID, entry.th32ParentProcessID));
            if unsafe { Process32NextW(snapshot, &mut entry) }.is_err() {
                break;
            }
        }
    }
    let _ = unsafe { CloseHandle(snapshot) };

    let mut tree = HashSet::from([root.0]);
    loop {
        let before = tree.len();
        for &(process, parent) in &parents {
            if tree.contains(&parent) {
                tree.insert(process);
            }
        }
        if tree.len() == before {
            break;
        }
    }
    let mut tree = tree.into_iter().map(ProcessId).collect::<Vec<_>>();
    tree.sort_by_key(|process| process.0);
    Ok(tree)
}

#[cfg(windows)]
fn first_process_thread(process_id: ProcessId) -> std::io::Result<u32> {
    use std::mem;
    use windows::Win32::{
        Foundation::CloseHandle,
        System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        },
    };
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }
        .map_err(|_| std::io::Error::last_os_error())?;
    let mut entry = THREADENTRY32 {
        dwSize: mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut found = None;
    if unsafe { Thread32First(snapshot, &mut entry) }.is_ok() {
        loop {
            if entry.th32OwnerProcessID == process_id.0 {
                found = Some(entry.th32ThreadID);
                break;
            }
            if unsafe { Thread32Next(snapshot, &mut entry) }.is_err() {
                break;
            }
        }
    }
    let _ = unsafe { CloseHandle(snapshot) };
    found
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "target thread not found"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Threading::GetCurrentProcess;

    #[test]
    fn detects_current_process_architecture() {
        let handle = unsafe { GetCurrentProcess() };
        let expected = if cfg!(target_arch = "aarch64") {
            ProcessArchitecture::Arm64
        } else {
            ProcessArchitecture::X86_64
        };
        assert_eq!(process_architecture(handle.0 as isize).unwrap(), expected);
        assert_eq!(
            executable_architecture(&std::env::current_exe().unwrap()).unwrap(),
            expected
        );
    }

    #[test]
    fn process_tree_contains_its_root() {
        let root = ProcessId(std::process::id());
        assert!(process_tree_ids(root).unwrap().contains(&root));
    }
}
