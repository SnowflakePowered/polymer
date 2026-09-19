#[cfg(windows)]
pub struct KillOnCloseJob {
    handle: std::os::windows::io::OwnedHandle,
}

#[cfg(windows)]
impl KillOnCloseJob {
    pub fn new() -> std::io::Result<Self> {
        use std::os::windows::io::FromRawHandle;
        use windows::Win32::System::JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        let handle =
            unsafe { CreateJobObjectW(None, None) }.map_err(|_| std::io::Error::last_os_error())?;
        let job = Self {
            handle: unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle.0) },
        };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        unsafe {
            SetInformationJobObject(
                job.raw_handle(),
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        }
        .map_err(|_| std::io::Error::last_os_error())?;
        Ok(job)
    }

    pub fn assign_process(&self, process: isize) -> std::io::Result<()> {
        use windows::Win32::System::JobObjects::AssignProcessToJobObject;

        unsafe {
            AssignProcessToJobObject(
                self.raw_handle(),
                windows::Win32::Foundation::HANDLE(process as *mut _),
            )
        }
        .map_err(|_| std::io::Error::last_os_error())
    }

    /// Immediately terminates every process assigned to this job.
    ///
    /// Kill-on-close remains the crash-safety fallback, but an intentional
    /// emulator shutdown must not depend on this process owning the last job
    /// handle before it waits for the target to exit.
    pub fn terminate(&self, exit_code: u32) -> std::io::Result<()> {
        use windows::Win32::System::JobObjects::TerminateJobObject;

        unsafe { TerminateJobObject(self.raw_handle(), exit_code) }
            .map_err(|_| std::io::Error::last_os_error())
    }

    fn raw_handle(&self) -> windows::Win32::Foundation::HANDLE {
        use std::os::windows::io::AsRawHandle;
        windows::Win32::Foundation::HANDLE(self.handle.as_raw_handle())
    }
}
