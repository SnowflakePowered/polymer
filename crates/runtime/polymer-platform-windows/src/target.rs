use crate::{
    IsolatedProcess, KillOnCloseJob, PrivateDesktop, ProcessArchitecture, ProcessId,
    process_architecture, resume_initial_thread,
};
use std::{
    ffi::{CStr, OsString},
    path::{Path, PathBuf},
    process::{Child, Command},
};

/// One DLL and installer export to invoke before a target's initial thread is
/// resumed. Relative DLL paths are resolved inside the target architecture's
/// artifact directory.
#[derive(Clone, Debug)]
pub struct InjectionLibrary {
    name: String,
    library: PathBuf,
    export: &'static CStr,
}

impl InjectionLibrary {
    pub fn new(
        name: impl Into<String>,
        library: impl Into<PathBuf>,
        export: &'static CStr,
    ) -> Self {
        Self {
            name: name.into(),
            library: library.into(),
            export,
        }
    }
}

enum TargetChild {
    Standard(Child),
    Isolated(IsolatedProcess),
}

impl TargetChild {
    fn id(&self) -> u32 {
        match self {
            Self::Standard(child) => child.id(),
            Self::Isolated(child) => child.id(),
        }
    }

    fn raw_handle(&self) -> isize {
        use std::os::windows::io::AsRawHandle;

        match self {
            Self::Standard(child) => child.as_raw_handle() as isize,
            Self::Isolated(child) => child.raw_handle(),
        }
    }

    fn duplicate_wait_handle(&self) -> std::io::Result<std::os::windows::io::OwnedHandle> {
        use std::os::windows::io::{BorrowedHandle, RawHandle};

        // SAFETY: the child owns this handle for the duration of the clone.
        let handle = unsafe { BorrowedHandle::borrow_raw(self.raw_handle() as RawHandle) };
        handle.try_clone_to_owned()
    }

    fn kill(&mut self) -> std::io::Result<()> {
        match self {
            Self::Standard(child) => child.kill(),
            Self::Isolated(child) => child.kill(),
        }
    }

    fn try_wait(&mut self) -> std::io::Result<Option<()>> {
        match self {
            Self::Standard(child) => child.try_wait().map(|status| status.map(drop)),
            Self::Isolated(child) => child.try_wait(),
        }
    }

    fn wait(&mut self) -> std::io::Result<()> {
        match self {
            Self::Standard(child) => child.wait().map(drop),
            Self::Isolated(child) => child.wait(),
        }
    }
}

/// A launched target and every resource that must remain alive with it.
///
/// The process starts suspended, is assigned to a kill-on-close job, receives
/// all requested injections, and is only then resumed.
pub struct TargetProcess {
    child: TargetChild,
    job: KillOnCloseJob,
    desktop: Option<PrivateDesktop>,
    architecture: ProcessArchitecture,
    retained: Vec<Box<dyn Send>>,
}

/// An independently owned handle that can only be used to observe target exit.
pub struct ProcessWaitHandle(std::os::windows::io::OwnedHandle);

impl ProcessWaitHandle {
    pub fn wait(&self) -> std::io::Result<()> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::{
            Foundation::{HANDLE, WAIT_FAILED, WAIT_OBJECT_0},
            System::Threading::{INFINITE, WaitForSingleObject},
        };

        let handle = HANDLE(self.0.as_raw_handle());
        match unsafe { WaitForSingleObject(handle, INFINITE) } {
            WAIT_OBJECT_0 => Ok(()),
            WAIT_FAILED => Err(std::io::Error::last_os_error()),
            result => Err(std::io::Error::other(format!(
                "unexpected process wait result {result:?}"
            ))),
        }
    }

    pub fn exit_code(&self) -> std::io::Result<u32> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::{Foundation::HANDLE, System::Threading::GetExitCodeProcess};

        let mut exit_code = 0;
        unsafe { GetExitCodeProcess(HANDLE(self.0.as_raw_handle()), &mut exit_code) }
            .map_err(std::io::Error::other)?;
        Ok(exit_code)
    }
}

impl TargetProcess {
    pub fn launch(
        target: &[String],
        artifact_root: &Path,
        environment: &[(OsString, OsString)],
        private_desktop: Option<&str>,
        libraries: &[InjectionLibrary],
    ) -> std::io::Result<Self> {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;

        let executable = target.first().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "target command is empty")
        })?;
        let mut command = Command::new(executable);
        command.args(&target[1..]);
        for (name, value) in environment {
            command.env(name, value);
        }
        command.creation_flags(CREATE_SUSPENDED);

        let job = KillOnCloseJob::new()?;
        let (desktop, mut child) = if let Some(name) = private_desktop {
            let desktop = PrivateDesktop::create(name)?;
            let child = IsolatedProcess::spawn_suspended(target, environment, name)?;
            (Some(desktop), TargetChild::Isolated(child))
        } else {
            (None, TargetChild::Standard(command.spawn()?))
        };
        if let Err(error) = job.assign_process(child.raw_handle()) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        if let Err(error) = force_high_qos(child.raw_handle()) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        let architecture = match process_architecture(child.raw_handle()) {
            Ok(architecture) => architecture,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        let mut process = Self {
            child,
            job,
            desktop,
            architecture,
            retained: Vec::new(),
        };
        let setup = (|| {
            let artifact_dir = architecture_artifact_dir(artifact_root, process.architecture);
            let libraries = resolve_libraries(&artifact_dir, libraries)?;
            if !libraries.is_empty() {
                run_injector(
                    artifact_root,
                    &artifact_dir,
                    process.architecture,
                    ProcessId(process.child.id()),
                    &libraries,
                )?;
            }
            resume_initial_thread(ProcessId(process.child.id()))
        })();
        if let Err(error) = setup {
            let _ = process.child.kill();
            let _ = process.child.wait();
            return Err(error);
        }
        Ok(process)
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    pub const fn architecture(&self) -> ProcessArchitecture {
        self.architecture
    }

    pub fn wait_handle(&self) -> std::io::Result<ProcessWaitHandle> {
        self.child.duplicate_wait_handle().map(ProcessWaitHandle)
    }

    pub fn try_wait(&mut self) -> std::io::Result<Option<()>> {
        self.child.try_wait()
    }

    /// Retain a resource for at least as long as the target. This is useful for
    /// temporary loader manifests and other launch-time files.
    pub fn retain(&mut self, resource: impl Send + 'static) {
        self.retained.push(Box::new(resource));
    }

    /// Close the job, terminating the root and all descendants, then wait for
    /// the root process handle to signal.
    pub fn terminate_tree_and_wait(self) -> std::io::Result<()> {
        let Self {
            mut child,
            job,
            desktop,
            architecture: _,
            retained,
        } = self;
        let job_termination = job.terminate(0);
        // Some emulator launch configurations keep an additional job handle
        // alive, so terminating the job alone is not a sufficient guarantee
        // that its root process has observed shutdown. Terminate the root via
        // its owned process handle as well before entering the blocking wait.
        let process_termination = child.kill();
        drop(job);
        let result = child.wait();
        drop(desktop);
        drop(retained);
        if job_termination.is_err() && process_termination.is_err() {
            job_termination.and(process_termination).and(result)
        } else {
            result
        }
    }
}

/// Explicitly opts a process out of EcoQoS and timer-resolution throttling.
///
/// This runs while the root process is suspended, before injection and before
/// any emulator code can create worker threads or child processes.
fn force_high_qos(process: isize) -> std::io::Result<()> {
    use windows::Win32::{
        Foundation::HANDLE,
        System::Threading::{
            PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION, PROCESS_POWER_THROTTLING_STATE,
            ProcessPowerThrottling, SetProcessInformation,
        },
    };

    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED
            | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
        StateMask: 0,
    };
    unsafe {
        SetProcessInformation(
            HANDLE(process as *mut _),
            ProcessPowerThrottling,
            (&raw const state).cast(),
            std::mem::size_of_val(&state) as u32,
        )
    }
    .map_err(|_| std::io::Error::last_os_error())
}

pub fn architecture_artifact_dir(
    artifact_root: &Path,
    architecture: ProcessArchitecture,
) -> PathBuf {
    let packaged = artifact_root.join(architecture.executable_suffix());
    if packaged.is_dir() {
        return packaged;
    }
    let target = match architecture {
        ProcessArchitecture::X86_64 => "x86_64-pc-windows-msvc",
        ProcessArchitecture::Arm64 => "aarch64-pc-windows-msvc",
    };
    let cargo = artifact_root
        .parent()
        .zip(artifact_root.file_name())
        .map(|(target_dir, profile)| target_dir.join(target).join(profile));
    cargo
        .filter(|directory| directory.is_dir())
        .unwrap_or_else(|| artifact_root.to_path_buf())
}

fn resolve_libraries(
    artifact_dir: &Path,
    libraries: &[InjectionLibrary],
) -> std::io::Result<Vec<InjectionLibrary>> {
    libraries
        .iter()
        .map(|library| {
            let path = if library.library.is_absolute() {
                library.library.clone()
            } else {
                artifact_dir.join(&library.library)
            };
            if !path.is_file() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("{} hook is missing at {}", library.name, path.display()),
                ));
            }
            Ok(InjectionLibrary {
                name: library.name.clone(),
                library: path,
                export: library.export,
            })
        })
        .collect()
}

fn run_injector(
    artifact_root: &Path,
    artifact_dir: &Path,
    architecture: ProcessArchitecture,
    process_id: ProcessId,
    libraries: &[InjectionLibrary],
) -> std::io::Result<()> {
    let suffixed = artifact_root.join(format!(
        "polymer-injector-{}.exe",
        architecture.executable_suffix()
    ));
    let architecture_local = artifact_root
        .join(architecture.executable_suffix())
        .join("polymer-injector.exe");
    let target_local = artifact_dir.join("polymer-injector.exe");
    let generic = artifact_root.join("polymer-injector.exe");
    let injector = if suffixed.is_file() {
        suffixed.clone()
    } else if architecture_local.is_file() {
        architecture_local
    } else if target_local.is_file() {
        target_local
    } else if generic.is_file()
        && matches!(
            (std::env::consts::ARCH, architecture),
            ("x86_64", ProcessArchitecture::X86_64) | ("aarch64", ProcessArchitecture::Arm64)
        )
    {
        generic
    } else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "{} injector is missing at {}",
                architecture.executable_suffix(),
                suffixed.display()
            ),
        ));
    };
    let mut command = Command::new(&injector);
    command.arg("--pid").arg(process_id.0.to_string());
    for library in libraries {
        command
            .arg("--library")
            .arg(&library.library)
            .arg("--export")
            .arg(library.export.to_string_lossy().as_ref());
    }
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "{} failed with {status}",
            injector.display()
        )))
    }
}
