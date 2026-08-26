use std::{
    io::{self, BufRead, BufReader},
    os::windows::process::CommandExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
};

use windows::Win32::System::Threading::CREATE_NO_WINDOW;

pub struct GameBarWatcher {
    // Retaining the handle lets us report premature watcher exits in a future
    // health check. Dropping Child does not terminate the process on Windows.
    _child: Child,
}

impl GameBarWatcher {
    pub fn launch() -> io::Result<Self> {
        let executable = watcher_executable()?;
        let mut child = Command::new(&executable)
            .arg("--parent-pid")
            .arg(std::process::id().to_string())
            .stdin(Stdio::null())
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
        let stdout = child.stdout.take().ok_or_else(|| {
            io::Error::other("snowglass-watcher did not expose its readiness pipe")
        })?;
        BufReader::new(stdout).read_line(&mut response)?;
        if response.trim_end() != "READY" {
            let status = child.try_wait()?;
            return Err(io::Error::other(format!(
                "snowglass-watcher failed before it became ready{}",
                status.map_or_else(String::new, |status| format!(" ({status})"))
            )));
        }

        Ok(Self { _child: child })
    }
}

fn watcher_executable() -> io::Result<PathBuf> {
    let current = std::env::current_exe()?;
    Ok(current.with_file_name("snowglass-watcher.exe"))
}
