use super::{ElevatedTask, InstallEvent, ProgressPipe, SetupState};
use eframe::egui;
use parking_lot::Mutex;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    net::windows::named_pipe::{NamedPipeServer, ServerOptions},
};
use windows::{
    Win32::{
        Foundation::{ERROR_CANCELLED, HANDLE},
        System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject},
        UI::{
            Shell::{
                SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
            },
            WindowsAndMessaging::SW_HIDE,
        },
    },
    core::{HSTRING, PCWSTR, w},
};

pub(super) struct ElevatedInstall {
    process: OwnedHandle,
}

impl ElevatedInstall {
    pub(super) async fn run(
        task: &ElevatedTask,
        state: &Mutex<SetupState>,
        ctx: &egui::Context,
    ) -> Result<u32, String> {
        let parent = std::process::id();
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .access_outbound(false)
            .create(ProgressPipe::name(parent))
            .map_err(|error| format!("Could not start the installer: {error}"))?;
        let arguments = task.arguments(parent);
        let install = match tokio::task::spawn_blocking(move || Self::launch(&arguments)).await {
            Ok(Ok(install)) => install,
            Ok(Err(error)) if error.code() == ERROR_CANCELLED.to_hresult() => {
                return Err("Administrator permission is needed to install this.".to_owned());
            }
            Ok(Err(error)) => return Err(format!("Could not start the installer: {error}")),
            Err(error) => return Err(error.to_string()),
        };
        let mut exit = tokio::task::spawn_blocking(move || install.wait());
        let connected = tokio::select! {
            code = &mut exit => return code.map_err(|error| error.to_string()),
            connected = server.connect() => connected,
        };
        if connected.is_ok() {
            Self::relay(server, state, ctx).await;
        }
        exit.await.map_err(|error| error.to_string())
    }

    fn launch(arguments: &str) -> windows::core::Result<Self> {
        let executable = HSTRING::from(std::env::current_exe()?.as_os_str());
        let parameters = HSTRING::from(arguments);
        let mut info = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
            lpVerb: w!("runas"),
            lpFile: PCWSTR(executable.as_ptr()),
            lpParameters: PCWSTR(parameters.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        unsafe { ShellExecuteExW(&mut info)? };
        Ok(Self {
            process: unsafe { OwnedHandle::from_raw_handle(info.hProcess.0) },
        })
    }

    fn wait(self) -> u32 {
        let process = HANDLE(self.process.as_raw_handle());
        let mut code = 0;
        unsafe {
            WaitForSingleObject(process, INFINITE);
            GetExitCodeProcess(process, &mut code).map_or(u32::MAX, |()| code)
        }
    }

    async fn relay(server: NamedPipeServer, state: &Mutex<SetupState>, ctx: &egui::Context) {
        let mut lines = BufReader::new(server).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Ok(event) = serde_json::from_str::<InstallEvent>(&line) {
                state.lock().apply(event);
                ctx.request_repaint();
            }
        }
    }
}
