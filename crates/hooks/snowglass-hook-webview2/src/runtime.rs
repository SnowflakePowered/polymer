//! Carries the graphics hooks into WebView2's child processes.
//!
//! The DXGI hook detours factory creation, so it only sees swapchains built
//! after it is installed. WebView2 creates its factory and its presentation
//! swapchain during startup, which means a process injected once it is already
//! running is always too late: the swapchain it presents was created before the
//! detours existed. The only way to be in time is to be there at creation, so
//! this hook forces every WebView2 child to start suspended, injects into it,
//! and only then lets it run.

use std::ffi::{OsString, c_void};
use std::os::windows::ffi::OsStringExt;
use windows_sys::Win32::{
    Security::SECURITY_ATTRIBUTES,
    System::Threading::{
        CREATE_SUSPENDED, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, ResumeThread,
        STARTUPINFOEXW, STARTUPINFOW,
    },
};
use windows_sys::core::{BOOL, PCWSTR, PWSTR};

/// Directory holding the injector and the hook libraries to propagate.
pub const ARTIFACT_DIR: &str = "SNOWGLASS_WEBVIEW2_ARTIFACT_DIR";

/// Desktop every WebView2 process must be placed on.
///
/// A child normally inherits the desktop of the thread that spawns it, but
/// WebView2 starts its browser process from a worker thread of its own, which
/// is still on the default desktop however the host thread is attached. The
/// browser would then be on a different desktop from the window it has to
/// parent into, which it refuses. Naming the desktop in `STARTUPINFOW` is what
/// puts it where the window is.
pub const DESKTOP: &str = "SNOWGLASS_WEBVIEW2_DESKTOP";

/// Image names whose children are worth carrying the hooks into. Anything else
/// a host process spawns is left completely alone.
const PROPAGATE_TO: &[&str] = &["msedgewebview2.exe"];

/// Libraries injected into each matching child, in order.
///
/// The DXGI hook only intercepts presentation; the backend hooks are what
/// actually claim a swapchain and publish its frames, and Chromium's GPU
/// process renders through D3D11. The process-creation hook goes in too, so a
/// browser process carries the chain on to the GPU process it spawns in turn.
const LIBRARIES: &[(&str, &str)] = &[
    ("snowglass_hook_dxgi.dll", "SnowglassInstallDxgiHook"),
    ("snowglass_hook_d3d11.dll", "SnowglassInstallD3d11Hook"),
    ("snowglass_hook_d3d12.dll", "SnowglassInstallD3d12Hook"),
    (
        "snowglass_hook_webview2.dll",
        "SnowglassInstallWebView2Hook",
    ),
];

enum RedirectedStartupInfo {
    Basic(STARTUPINFOW),
    Extended(STARTUPINFOEXW),
}

impl RedirectedStartupInfo {
    unsafe fn new(source: *const STARTUPINFOW, desktop: PWSTR) -> Self {
        // WebView2 supplies STARTUPINFOEXW even when its creation flags do not
        // include EXTENDED_STARTUPINFO_PRESENT. Preserve the trailing
        // attribute-list pointer: copying only STARTUPINFOW silently drops it
        // and the browser later rejects the controller's HWND.
        if unsafe { (*source).cb } as usize >= size_of::<STARTUPINFOEXW>() {
            // SAFETY: `cb` says the caller supplied the extended structure.
            let mut redirected = unsafe { *source.cast::<STARTUPINFOEXW>() };
            redirected.StartupInfo.lpDesktop = desktop;
            Self::Extended(redirected)
        } else {
            // SAFETY: a non-null CreateProcessW startup structure always
            // contains at least its declared STARTUPINFOW base.
            let mut redirected = unsafe { *source };
            redirected.lpDesktop = desktop;
            Self::Basic(redirected)
        }
    }

    fn as_ptr(&self) -> *const STARTUPINFOW {
        match self {
            Self::Basic(info) => info,
            Self::Extended(info) => (&raw const *info).cast(),
        }
    }
}

pub(crate) unsafe fn create_process(
    application_name: PCWSTR,
    command_line: PWSTR,
    process_attributes: *const SECURITY_ATTRIBUTES,
    thread_attributes: *const SECURITY_ATTRIBUTES,
    inherit_handles: BOOL,
    creation_flags: PROCESS_CREATION_FLAGS,
    environment: *const c_void,
    current_directory: PCWSTR,
    startup_info: *const STARTUPINFOW,
    process_information: *mut PROCESS_INFORMATION,
    mut next: crate::hooks::CreateProcessContext<'_>,
) -> BOOL {
    let wanted = unsafe { should_propagate(application_name, command_line) };
    // Suspending a process that the caller expected to be running would change
    // the behaviour of every unrelated spawn, so only the children being hooked
    // are forced, and they are resumed as soon as the injection is done.
    let already_suspended = creation_flags & CREATE_SUSPENDED != 0;
    let flags = if wanted {
        creation_flags | CREATE_SUSPENDED
    } else {
        creation_flags
    };
    // Redirect the child onto the desktop the page is hosted on, overriding
    // whatever the spawning thread happened to be attached to.
    let desktop = wanted
        .then(|| std::env::var_os(DESKTOP))
        .flatten()
        .map(|name| {
            name.to_string_lossy()
                .encode_utf16()
                .chain(Some(0))
                .collect::<Vec<_>>()
        });
    let redirected = desktop
        .as_ref()
        .filter(|_| !startup_info.is_null())
        // SAFETY: the non-null structure belongs to this CreateProcessW call,
        // and the redirected copy remains alive until the trampoline returns.
        .map(|name| unsafe { RedirectedStartupInfo::new(startup_info, name.as_ptr().cast_mut()) });
    let startup_info = redirected
        .as_ref()
        .map_or(startup_info, RedirectedStartupInfo::as_ptr);
    let result = snowglass_hook::next!(
        next,
        application_name,
        command_line,
        process_attributes,
        thread_attributes,
        inherit_handles,
        flags,
        environment,
        current_directory,
        startup_info,
        process_information
    );
    if result == 0 || !wanted || process_information.is_null() {
        return result;
    }
    // SAFETY: `CreateProcessW` filled this out on success.
    let information = unsafe { &*process_information };
    inject(information.dwProcessId);
    if !already_suspended {
        // SAFETY: the handle belongs to the process just created, and resuming
        // restores exactly what the caller asked for.
        unsafe { ResumeThread(information.hThread) };
    }
    result
}

/// Whether a spawn is one of WebView2's own processes.
unsafe fn should_propagate(application_name: PCWSTR, command_line: PWSTR) -> bool {
    let image = unsafe { wide_string(application_name) }
        .or_else(|| unsafe { wide_string(command_line.cast_const()) })
        .unwrap_or_default()
        .to_ascii_lowercase();
    if image.is_empty() {
        return false;
    }
    PROPAGATE_TO.iter().any(|name| image.contains(name))
}

unsafe fn wide_string(value: PCWSTR) -> Option<String> {
    if value.is_null() {
        return None;
    }
    let mut length = 0;
    // SAFETY: the caller passes a null-terminated wide string, and the scan
    // stops at the terminator or a sane bound.
    unsafe {
        while length < 32_768 && *value.add(length) != 0 {
            length += 1;
        }
        Some(
            OsString::from_wide(std::slice::from_raw_parts(value, length))
                .to_string_lossy()
                .into_owned(),
        )
    }
}

/// Runs the injector against a suspended child.
///
/// Shelling out keeps the remote-thread work in the one place that already
/// does it. The image filter means the injector's own spawn cannot match and
/// recurse back into this hook.
fn inject(process_id: u32) {
    let Some(directory) = std::env::var_os(ARTIFACT_DIR) else {
        log("no artifact directory, so the child was left unhooked");
        return;
    };
    let directory = std::path::PathBuf::from(directory);
    let injector = directory.join("snowglass-injector.exe");
    if !injector.is_file() {
        log(format!("injector is missing at {}", injector.display()));
        return;
    }
    let mut command = std::process::Command::new(&injector);
    command.arg("--pid").arg(process_id.to_string());
    for (library, export) in LIBRARIES {
        let path = directory.join(library);
        if !path.is_file() {
            continue;
        }
        command.arg("--library").arg(&path).arg("--export").arg(export);
    }
    match command.status() {
        Ok(status) if status.success() => log(format!("hooked WebView2 child {process_id}")),
        Ok(status) => log(format!("injecting into {process_id} failed: {status}")),
        Err(error) => log(format!("could not run the injector for {process_id}: {error}")),
    }
}

fn log(message: impl std::fmt::Display) {
    eprintln!("snowglass: webview2 hook: {message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirecting_extended_startup_info_preserves_its_attribute_list() {
        let mut source = STARTUPINFOEXW::default();
        source.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        source.lpAttributeList = 0x1234usize as _;
        let mut desktop = "snowglass-test\0".encode_utf16().collect::<Vec<_>>();

        let redirected =
            unsafe { RedirectedStartupInfo::new((&raw const source).cast(), desktop.as_mut_ptr()) };

        let RedirectedStartupInfo::Extended(redirected) = redirected else {
            panic!("extended startup info was truncated to its base");
        };
        assert_eq!(redirected.StartupInfo.lpDesktop, desktop.as_mut_ptr());
        assert_eq!(redirected.lpAttributeList, source.lpAttributeList);
    }

    #[test]
    fn redirecting_basic_startup_info_changes_only_the_desktop() {
        let mut source = STARTUPINFOW {
            cb: size_of::<STARTUPINFOW>() as u32,
            dwX: 37,
            ..Default::default()
        };
        source.lpTitle = 0x5678usize as _;
        let mut desktop = "snowglass-test\0".encode_utf16().collect::<Vec<_>>();

        let redirected =
            unsafe { RedirectedStartupInfo::new(&raw const source, desktop.as_mut_ptr()) };

        let RedirectedStartupInfo::Basic(redirected) = redirected else {
            panic!("basic startup info was treated as extended");
        };
        assert_eq!(redirected.lpDesktop, desktop.as_mut_ptr());
        assert_eq!(redirected.lpTitle, source.lpTitle);
        assert_eq!(redirected.dwX, source.dwX);
    }
}
