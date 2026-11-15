use std::{
    io::{self, BufRead, BufReader},
    os::windows::{io::AsRawHandle, process::CommandExt},
    path::PathBuf,
    process::{Child, Command, Stdio},
};

use windows::Win32::{
    Foundation::HANDLE,
    System::Threading::{CREATE_NO_WINDOW, WaitForSingleObject},
};

pub struct GameBarWatcher {
    child: Child,
}

impl GameBarWatcher {
    const SHUTDOWN_TIMEOUT_MS: u32 = 5_000;

    pub fn launch() -> io::Result<Self> {
        let executable = watcher_executable()?;
        let mut child = Command::new(&executable)
            .arg("--parent-pid")
            .arg(std::process::id().to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .creation_flags(CREATE_NO_WINDOW.0)
            .spawn()
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("failed to launch {}: {error}", executable.display()),
                )
            })?;

        let mut response = String::new();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("polymer-watcher did not expose its readiness pipe"))?;
        BufReader::new(stdout).read_line(&mut response)?;
        if response.trim_end() != "READY" {
            let status = child.try_wait()?;
            return Err(io::Error::other(format!(
                "polymer-watcher failed before it became ready{}",
                status.map_or_else(String::new, |status| format!(" ({status})"))
            )));
        }

        Ok(Self { child })
    }
}

impl Drop for GameBarWatcher {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let process = HANDLE(self.child.as_raw_handle());
        unsafe { WaitForSingleObject(process, Self::SHUTDOWN_TIMEOUT_MS) };
    }
}

fn watcher_executable() -> io::Result<PathBuf> {
    let current = std::env::current_exe()?;
    Ok(current.with_file_name("polymer-watcher.exe"))
}
