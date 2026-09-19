pub(super) struct ProcessHandle {
    process: std::os::windows::io::OwnedHandle,
    process_id: u32,
}

impl ProcessHandle {
    pub(super) fn spawn_suspended(
        target: &[String],
        environment: &[(std::ffi::OsString, std::ffi::OsString)],
        desktop: Option<&str>,
    ) -> std::io::Result<Self> {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;

        if let Some(desktop) = desktop {
            return Self::spawn_isolated(target, environment, desktop);
        }
        let executable = target.first().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "target command is empty")
        })?;
        let child = std::process::Command::new(executable)
            .args(&target[1..])
            .envs(environment.iter().cloned())
            .creation_flags(CREATE_SUSPENDED)
            .spawn()?;
        Ok(Self {
            process_id: child.id(),
            process: child.into(),
        })
    }

    fn spawn_isolated(
        target: &[String],
        environment_overrides: &[(std::ffi::OsString, std::ffi::OsString)],
        desktop: &str,
    ) -> std::io::Result<Self> {
        use std::os::windows::io::FromRawHandle;
        use windows::Win32::System::Threading::{
            CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, PROCESS_INFORMATION,
            STARTUPINFOW,
        };
        use windows::core::{PCWSTR, PWSTR};

        let mut command_line = Self::command_line(target)?;
        let environment = Self::environment(environment_overrides)?;
        let mut desktop = desktop.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        let startup = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            lpDesktop: PWSTR(desktop.as_mut_ptr()),
            ..Default::default()
        };
        let mut process_info = PROCESS_INFORMATION::default();
        unsafe {
            CreateProcessW(
                PCWSTR::null(),
                Some(PWSTR(command_line.as_mut_ptr())),
                None,
                None,
                false,
                CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
                Some(environment.as_ptr().cast()),
                PCWSTR::null(),
                &startup,
                &raw mut process_info,
            )
        }
        .map_err(|_| std::io::Error::last_os_error())?;
        let process =
            unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(process_info.hProcess.0) };
        let initial_thread =
            unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(process_info.hThread.0) };
        drop(initial_thread);
        Ok(Self {
            process,
            process_id: process_info.dwProcessId,
        })
    }

    pub(super) const fn id(&self) -> u32 {
        self.process_id
    }

    pub(super) fn raw_handle(&self) -> isize {
        use std::os::windows::io::AsRawHandle;
        self.process.as_raw_handle() as isize
    }

    pub(super) fn kill(&mut self) -> std::io::Result<()> {
        unsafe {
            windows::Win32::System::Threading::TerminateProcess(
                windows::Win32::Foundation::HANDLE(self.raw_handle() as *mut _),
                1,
            )
        }
        .map_err(|_| std::io::Error::last_os_error())
    }

    pub(super) fn wait(&self) -> std::io::Result<()> {
        use windows::Win32::{
            Foundation::{WAIT_FAILED, WAIT_OBJECT_0},
            System::Threading::{INFINITE, WaitForSingleObject},
        };
        let handle = windows::Win32::Foundation::HANDLE(self.raw_handle() as *mut _);
        match unsafe { WaitForSingleObject(handle, INFINITE) } {
            WAIT_OBJECT_0 => Ok(()),
            WAIT_FAILED => Err(std::io::Error::last_os_error()),
            result => Err(std::io::Error::other(format!(
                "unexpected process wait result {result:?}"
            ))),
        }
    }

    fn append_argument(
        command_line: &mut Vec<u16>,
        value: &std::ffi::OsStr,
        always_quote: bool,
    ) -> std::io::Result<()> {
        use std::os::windows::ffi::OsStrExt;
        let value = value.encode_wide().collect::<Vec<_>>();
        if value.contains(&0) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "process argument contains a null character",
            ));
        }
        let quote = always_quote
            || value.is_empty()
            || value
                .iter()
                .any(|unit| *unit == b' ' as u16 || *unit == b'\t' as u16);
        if !quote {
            command_line.extend_from_slice(&value);
            return Ok(());
        }
        command_line.push(b'"' as u16);
        let mut backslashes = 0;
        for unit in value {
            if unit == b'\\' as u16 {
                backslashes += 1;
            } else if unit == b'"' as u16 {
                command_line.extend(std::iter::repeat_n(b'\\' as u16, backslashes * 2 + 1));
                command_line.push(unit);
                backslashes = 0;
            } else {
                command_line.extend(std::iter::repeat_n(b'\\' as u16, backslashes));
                command_line.push(unit);
                backslashes = 0;
            }
        }
        command_line.extend(std::iter::repeat_n(b'\\' as u16, backslashes * 2));
        command_line.push(b'"' as u16);
        Ok(())
    }

    fn command_line(target: &[String]) -> std::io::Result<Vec<u16>> {
        let executable = target.first().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "target command is empty")
        })?;
        let mut command_line = Vec::new();
        Self::append_argument(&mut command_line, std::ffi::OsStr::new(executable), true)?;
        for argument in &target[1..] {
            command_line.push(b' ' as u16);
            Self::append_argument(&mut command_line, std::ffi::OsStr::new(argument), false)?;
        }
        command_line.push(0);
        Ok(command_line)
    }

    fn environment(
        overrides: &[(std::ffi::OsString, std::ffi::OsString)],
    ) -> std::io::Result<Vec<u16>> {
        use std::os::windows::ffi::OsStrExt;
        let mut environment = std::env::vars_os().collect::<Vec<_>>();
        for (name, value) in overrides {
            if let Some(existing) = environment.iter_mut().find(|(existing, _)| {
                existing
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&name.to_string_lossy())
            }) {
                existing.1.clone_from(value);
            } else {
                environment.push((name.clone(), value.clone()));
            }
        }
        environment.sort_by_cached_key(|(name, _)| name.to_string_lossy().to_lowercase());
        let mut block = Vec::new();
        for (name, value) in environment {
            let name = name.encode_wide().collect::<Vec<_>>();
            let value = value.encode_wide().collect::<Vec<_>>();
            if name.contains(&0) || value.contains(&0) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "environment contains a null character",
                ));
            }
            block.extend(name);
            block.push(b'=' as u16);
            block.extend(value);
            block.push(0);
        }
        block.push(0);
        Ok(block)
    }

    pub(super) fn subscribe_exit(&self) -> std::io::Result<polymer_platform::ProcessExitReceiver> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::{Foundation::HANDLE, System::Threading::GetExitCodeProcess};

        let process = Self {
            process: self.process.try_clone()?,
            process_id: self.process_id,
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("polymer-windows-process-wait".to_owned())
            .spawn(move || {
                let exit = process.wait().and_then(|()| {
                    let mut code = 0;
                    // SAFETY: the cloned handle stays owned until the waiter exits.
                    unsafe {
                        GetExitCodeProcess(HANDLE(process.process.as_raw_handle()), &mut code)
                    }
                    .map_err(std::io::Error::other)?;
                    Ok(polymer_platform::ProcessExit::Exited { code })
                });
                let _ = sender.send(exit);
            })?;
        Ok(receiver)
    }
}
