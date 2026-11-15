use super::{InstallEvent, ProgressPipe, Setup, SetupState};
use eframe::egui;
use polymer_vfs::FilesystemHost;
use std::{
    ffi::c_void,
    fs::OpenOptions,
    io::{self, Read},
    os::windows::fs::OpenOptionsExt,
    path::PathBuf,
};
use windows::{
    Win32::{
        Storage::FileSystem::FILE_SHARE_READ,
        System::ApplicationInstallationAndServicing::{
            INSTALLLOGATTRIBUTES_FLUSHEACHLINE, INSTALLLOGMODE, INSTALLLOGMODE_ACTIONDATA,
            INSTALLLOGMODE_ACTIONSTART, INSTALLLOGMODE_COMMONDATA, INSTALLLOGMODE_ERROR,
            INSTALLLOGMODE_FATALEXIT, INSTALLLOGMODE_INFO, INSTALLLOGMODE_OUTOFDISKSPACE,
            INSTALLLOGMODE_PROGRESS, INSTALLLOGMODE_RESOLVESOURCE, INSTALLLOGMODE_USER,
            INSTALLLOGMODE_VERBOSE, INSTALLLOGMODE_WARNING, INSTALLMESSAGE,
            INSTALLMESSAGE_ACTIONDATA, INSTALLMESSAGE_ACTIONSTART, INSTALLMESSAGE_PROGRESS,
            INSTALLSTATE_DEFAULT, INSTALLUILEVEL_NONE, MSIHANDLE, MsiEnableLogW,
            MsiInstallProductW, MsiQueryProductStateW, MsiRecordGetInteger, MsiRecordGetStringW,
            MsiSetExternalUIRecord, MsiSetInternalUI,
        },
    },
    core::{HSTRING, PCWSTR, PWSTR, w},
};

const PACKAGE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/redist/winfsp-2.2.26286.msi"
));
const PACKAGE_NAME: &str = "winfsp-2.2.26286.msi";
const PRODUCT_CODE: PCWSTR = w!("{ED6CC0E6-A45E-4941-8E52-57D93B7CF423}");
const WINFSP_NOTICE: &str = "WinFsp - Windows File System Proxy, Copyright (C) Bill Zissimopoulos";
const WINFSP_REPOSITORY: &str = "https://github.com/winfsp/winfsp";
const ERROR_INSTALL_FAILURE: i32 = 1603;
const ERROR_INSTALL_USEREXIT: u32 = 1602;
const ERROR_INSTALL_ALREADY_RUNNING: u32 = 1618;
const ERROR_SUCCESS_REBOOT_INITIATED: u32 = 1641;
const ERROR_SUCCESS_REBOOT_REQUIRED: u32 = 3010;
const IDOK: i32 = 1;
const MESSAGE_TYPE_MASK: u32 = 0xFF00_0000;
const UI_MESSAGES: i32 =
    INSTALLLOGMODE_PROGRESS.0 | INSTALLLOGMODE_ACTIONSTART.0 | INSTALLLOGMODE_ACTIONDATA.0;
const LOG_MODE: INSTALLLOGMODE = INSTALLLOGMODE(
    INSTALLLOGMODE_FATALEXIT.0
        | INSTALLLOGMODE_ERROR.0
        | INSTALLLOGMODE_WARNING.0
        | INSTALLLOGMODE_USER.0
        | INSTALLLOGMODE_INFO.0
        | INSTALLLOGMODE_RESOLVESOURCE.0
        | INSTALLLOGMODE_OUTOFDISKSPACE.0
        | INSTALLLOGMODE_ACTIONSTART.0
        | INSTALLLOGMODE_ACTIONDATA.0
        | INSTALLLOGMODE_COMMONDATA.0
        | INSTALLLOGMODE_VERBOSE.0,
);

pub(super) enum WinFspAction {
    Install,
    Continue,
    Close,
}

#[derive(Default)]
struct Progress {
    total: i32,
    done: i32,
    backward: bool,
    scripting: bool,
    step: i32,
}

impl Progress {
    fn update(&mut self, record: MSIHANDLE) -> Option<InstallEvent> {
        let field = |index| unsafe { MsiRecordGetInteger(record, index) };
        match field(1) {
            0 => {
                self.total = field(2);
                self.backward = field(3) == 1;
                self.done = if self.backward { self.total } else { 0 };
                self.scripting = field(4) == 1;
            }
            1 => {
                self.step = if field(3) == 0 { 0 } else { field(2) };
                return None;
            }
            2 => self.advance(field(2)),
            3 => self.total += field(2),
            _ => return None,
        }
        self.report()
    }

    fn action_data(&mut self) -> Option<InstallEvent> {
        if self.step == 0 {
            return None;
        }
        self.advance(self.step);
        self.report()
    }

    fn advance(&mut self, ticks: i32) {
        self.done += if self.backward { -ticks } else { ticks };
    }

    fn report(&self) -> Option<InstallEvent> {
        (!self.scripting && self.total > 0).then(|| InstallEvent::Progress {
            fraction: (self.done as f32 / self.total as f32).clamp(0.0, 1.0),
        })
    }
}

pub(super) struct WinFspInstaller {
    pipe: ProgressPipe,
    progress: Progress,
}

impl WinFspInstaller {
    pub(super) fn run(parent: u32) -> i32 {
        let mut installer = Self {
            pipe: ProgressPipe::connect(parent),
            progress: Progress::default(),
        };
        match installer.install() {
            Ok(code) => code as i32,
            Err(error) => error.raw_os_error().unwrap_or(ERROR_INSTALL_FAILURE),
        }
    }

    pub(super) fn installed() -> bool {
        polymer_runtime::filesystem_host().supported()
    }

    pub(super) fn outcome(code: u32) -> SetupState {
        match code {
            0 => SetupState::Installed,
            ERROR_SUCCESS_REBOOT_REQUIRED | ERROR_SUCCESS_REBOOT_INITIATED => {
                SetupState::RestartRequired
            }
            ERROR_INSTALL_USEREXIT => {
                SetupState::Failed("The WinFsp installation was cancelled.".to_owned())
            }
            ERROR_INSTALL_ALREADY_RUNNING => SetupState::Failed(
                "Another installation is in progress. Wait for it to finish, then try again."
                    .to_owned(),
            ),
            code => SetupState::Failed(format!(
                "WinFsp could not be installed (error {code}). The installer log is at {}.",
                Self::log_path().display()
            )),
        }
    }

    pub(super) fn show(
        ui: &mut egui::Ui,
        state: &SetupState,
        confirm: bool,
        installed: bool,
    ) -> Option<WinFspAction> {
        ui.label(
            "Polymer uses WinFsp to give emulators their virtual file systems. \
             It is installed once and needs administrator permission.",
        );
        Setup::notice(ui, WINFSP_NOTICE, WINFSP_REPOSITORY);
        ui.add_space(8.0);
        state.show(ui);
        let label = match state {
            SetupState::Ready if installed => {
                ui.label("WinFsp is already installed.");
                "Reinstall WinFsp"
            }
            SetupState::Ready => "Install WinFsp",
            SetupState::Failed(_) => "Try again",
            SetupState::RestartRequired => {
                ui.label("Restart Windows to finish installing WinFsp, then open Polymer again.");
                return (Setup::button(ui, "Close", true, true) || confirm)
                    .then_some(WinFspAction::Close);
            }
            SetupState::Installing { .. } | SetupState::Installed => return None,
        };
        if Setup::button(ui, label, true, !installed) || (confirm && !installed) {
            return Some(WinFspAction::Install);
        }
        (installed && (Setup::button(ui, "Continue", true, true) || confirm))
            .then_some(WinFspAction::Continue)
    }

    fn log_path() -> PathBuf {
        std::env::temp_dir().join("polymer-winfsp-install.log")
    }

    fn install(&mut self) -> io::Result<u32> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join(PACKAGE_NAME);
        std::fs::write(&path, PACKAGE)?;
        let mut package = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ.0)
            .open(&path)?;
        let mut contents = Vec::with_capacity(PACKAGE.len());
        package.read_to_end(&mut contents)?;
        if contents != PACKAGE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the extracted WinFsp package was modified",
            ));
        }
        let path = HSTRING::from(path.as_os_str());
        let log = HSTRING::from(Self::log_path().as_os_str());
        let context = std::ptr::from_mut(self).cast::<c_void>();
        Ok(unsafe {
            MsiSetInternalUI(INSTALLUILEVEL_NONE, None);
            MsiSetExternalUIRecord(
                Some(Self::handle_message),
                UI_MESSAGES as u32,
                Some(context.cast_const()),
                None,
            );
            MsiEnableLogW(LOG_MODE, &log, INSTALLLOGATTRIBUTES_FLUSHEACHLINE.0 as u32);
            let options = if MsiQueryProductStateW(PRODUCT_CODE) == INSTALLSTATE_DEFAULT {
                w!("REINSTALL=ALL REINSTALLMODE=vamus REBOOT=ReallySuppress")
            } else {
                w!("REBOOT=ReallySuppress")
            };
            MsiInstallProductW(&path, options)
        })
    }

    unsafe extern "system" fn handle_message(
        context: *mut c_void,
        message: u32,
        record: MSIHANDLE,
    ) -> i32 {
        let installer = unsafe { &mut *context.cast::<Self>() };
        let event = match INSTALLMESSAGE((message & MESSAGE_TYPE_MASK) as i32) {
            INSTALLMESSAGE_PROGRESS => installer.progress.update(record),
            INSTALLMESSAGE_ACTIONDATA => installer.progress.action_data(),
            INSTALLMESSAGE_ACTIONSTART => Self::action(record),
            _ => return 0,
        };
        if let Some(event) = event {
            installer.pipe.send(&event);
        }
        IDOK
    }

    fn action(record: MSIHANDLE) -> Option<InstallEvent> {
        let mut buffer = [0u16; 256];
        let mut length = buffer.len() as u32;
        unsafe {
            MsiRecordGetStringW(
                record,
                2,
                Some(PWSTR(buffer.as_mut_ptr())),
                Some(&mut length),
            );
        }
        let length = (length as usize).min(buffer.len() - 1);
        let description = String::from_utf16_lossy(&buffer[..length]);
        (!description.is_empty()).then_some(InstallEvent::Action { description })
    }
}
