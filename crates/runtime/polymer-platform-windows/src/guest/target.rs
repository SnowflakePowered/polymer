use super::{GuestPlatform, injection::InjectionLibrary, process::ProcessHandle};
use crate::{
    KillOnCloseJob, PrivateDesktop, ProcessArchitecture, ProcessId, process_architecture,
    resume_initial_thread,
};
use std::{ffi::OsString, path::Path};

/// A launched target and every resource that must remain alive with it.
///
/// The process starts suspended, is assigned to a kill-on-close job, receives
/// all requested injections, and is only then resumed.
pub(super) struct TargetProcess {
    pub(super) child: ProcessHandle,
    job: KillOnCloseJob,
    desktop: Option<PrivateDesktop>,
    pub(super) architecture: ProcessArchitecture,
    pub(super) vulkan_manifest: Option<tempfile::TempDir>,
}

impl TargetProcess {
    pub(super) fn launch(
        target: &[String],
        artifact_root: &Path,
        environment: &[(OsString, OsString)],
        working_directory: Option<&Path>,
        private_desktop: Option<&str>,
        libraries: &[InjectionLibrary],
    ) -> std::io::Result<Self> {
        let job = KillOnCloseJob::new()?;
        let desktop = private_desktop.map(PrivateDesktop::create).transpose()?;
        let mut child = ProcessHandle::spawn_suspended(
            target,
            environment,
            working_directory,
            private_desktop,
        )?;
        let setup = (|| {
            job.assign_process(child.raw_handle())?;
            Self::force_high_qos(child.raw_handle())?;
            let architecture = process_architecture(child.raw_handle())?;
            let artifact_dir = GuestPlatform::artifact_dir(artifact_root, architecture);
            let libraries = InjectionLibrary::resolve(&artifact_dir, libraries)?;
            if !libraries.is_empty() {
                InjectionLibrary::inject(
                    artifact_root,
                    &artifact_dir,
                    architecture,
                    ProcessId(child.id()),
                    &libraries,
                )?;
            }
            resume_initial_thread(ProcessId(child.id()))?;
            Ok(architecture)
        })();
        let architecture = match setup {
            Ok(architecture) => architecture,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        Ok(Self {
            child,
            job,
            desktop,
            architecture,
            vulkan_manifest: None,
        })
    }

    /// Close the job, terminating the root and all descendants, then wait for
    /// the root process handle to signal.
    pub(super) fn terminate_tree_and_wait(self) -> std::io::Result<()> {
        let Self {
            mut child,
            job,
            desktop,
            architecture: _,
            vulkan_manifest,
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
        drop(vulkan_manifest);
        if job_termination.is_err() && process_termination.is_err() {
            job_termination.and(process_termination).and(result)
        } else {
            result
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
}
