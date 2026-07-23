//! Windows-specific handle, adapter, process, and window utilities.

static INLINE_HOOK_MUTATION: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A thread-safe owner for an `ib-hook` inline hook.
///
/// Hook state changes take an exclusive lock, while trampoline access only
/// takes a shared lock. This lets hook installers publish their hook storage
/// before enabling it, so a detour can always find its original trampoline.
#[derive(Debug)]
pub struct InlineDetour<F: ib_hook::FnPtr> {
    hook: std::sync::RwLock<ib_hook::inline::InlineHook<F>>,
}

impl<F: ib_hook::FnPtr> InlineDetour<F> {
    pub const fn new(target: F, detour: F) -> Self {
        Self {
            hook: std::sync::RwLock::new(ib_hook::inline::InlineHook::new(target, detour)),
        }
    }

    pub fn enable(&self) -> Result<(), i32> {
        let _mutation = INLINE_HOOK_MUTATION
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut hook = self.hook.write().unwrap_or_else(|error| error.into_inner());
        let result = hook.enable();
        if result.is_ok() {
            Ok(())
        } else {
            Err(result.0)
        }
    }

    pub fn disable(&self) -> Result<(), i32> {
        let _mutation = INLINE_HOOK_MUTATION
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut hook = self.hook.write().unwrap_or_else(|error| error.into_inner());
        let result = hook.disable();
        if result.is_ok() {
            Ok(())
        } else {
            Err(result.0)
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.hook
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .is_enabled()
    }

    pub fn trampoline(&self) -> F {
        self.hook
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .trampoline()
    }
}

impl<F: ib_hook::FnPtr> snowglass_hook::TerminalLifecycle for InlineDetour<F> {
    fn enable(&self) -> Result<(), i32> {
        InlineDetour::enable(self)
    }

    fn disable(&self) -> Result<(), i32> {
        InlineDetour::disable(self)
    }
}

impl<F: ib_hook::FnPtr> snowglass_hook::TerminalCallable for InlineDetour<F> {
    type Function = F;

    fn terminal_callable(&self) -> F {
        self.trampoline()
    }
}

impl<F: ib_hook::FnPtr> snowglass_hook::TerminalDetour<F> for InlineDetour<F> {
    fn create(target: F, detour: F) -> Self {
        Self::new(target, detour)
    }
}

/// Calls an installed detour's original trampoline, or returns `fallback` if
/// the detour is absent or disabled.
pub fn call_original<T, R>(
    detour: Option<&InlineDetour<T>>,
    fallback: R,
    call: impl FnOnce(&InlineDetour<T>) -> R,
) -> R
where
    T: ib_hook::FnPtr,
{
    match detour {
        Some(detour) if detour.is_enabled() => call(detour),
        Some(_) | None => fallback,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct ProcessId(pub u32);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct NativeWindow(pub isize);

#[cfg(windows)]
pub struct KillOnCloseJob {
    handle: std::os::windows::io::OwnedHandle,
}

#[cfg(windows)]
impl KillOnCloseJob {
    pub fn new() -> std::io::Result<Self> {
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let job = Self {
            handle: unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle) },
        };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                job.raw_handle(),
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(job)
    }

    pub fn assign_process(
        &self,
        process: windows_sys::Win32::Foundation::HANDLE,
    ) -> std::io::Result<()> {
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;

        if unsafe { AssignProcessToJobObject(self.raw_handle(), process) } == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn raw_handle(&self) -> windows_sys::Win32::Foundation::HANDLE {
        use std::os::windows::io::AsRawHandle;
        self.handle.as_raw_handle()
    }
}

#[cfg(windows)]
pub struct PrivateDesktop {
    handle: windows_sys::Win32::System::StationsAndDesktops::HDESK,
}

#[cfg(windows)]
pub struct DesktopThreadAttachment {
    original: windows_sys::Win32::System::StationsAndDesktops::HDESK,
    restored: bool,
}

// SAFETY: A desktop handle is not thread-affine once no thread is attached to it. The
// launch thread is restored before PrivateDesktop moves to the monitor thread,
// which only retains and eventually closes the handle.
#[cfg(windows)]
unsafe impl Send for PrivateDesktop {}

#[cfg(windows)]
impl PrivateDesktop {
    /// Creates a desktop without attaching the calling thread. This is the
    /// launch-safe variant for UI threads, which Windows does not allow to
    /// switch desktops after they have created windows or installed hooks.
    pub fn create(name: &str) -> std::io::Result<Self> {
        use windows_sys::Win32::{
            Foundation::GENERIC_ALL, Security::SECURITY_ATTRIBUTES,
            System::StationsAndDesktops::CreateDesktopW,
        };

        if name.encode_utf16().any(|unit| unit == 0) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "desktop name contains a null character",
            ));
        }
        let name = name.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: 0,
        };
        let handle = unsafe {
            CreateDesktopW(
                name.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                GENERIC_ALL,
                &raw const attributes,
            )
        };
        if handle.is_null() {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(Self { handle })
        }
    }

    pub fn create_and_attach_current_thread(
        name: &str,
    ) -> std::io::Result<(Self, DesktopThreadAttachment)> {
        use windows_sys::Win32::System::{
            StationsAndDesktops::{GetThreadDesktop, SetThreadDesktop},
            Threading::GetCurrentThreadId,
        };

        let original = unsafe { GetThreadDesktop(GetCurrentThreadId()) };
        if original.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let desktop = Self::create(name)?;
        if unsafe { SetThreadDesktop(desktop.handle) } == 0 {
            let error = std::io::Error::last_os_error();
            unsafe { SetThreadDesktop(original) };
            return Err(error);
        }
        Ok((
            desktop,
            DesktopThreadAttachment {
                original,
                restored: false,
            },
        ))
    }
}

#[cfg(windows)]
impl Drop for PrivateDesktop {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::System::StationsAndDesktops::CloseDesktop(self.handle) };
    }
}

#[cfg(windows)]
impl DesktopThreadAttachment {
    pub fn restore(&mut self) -> std::io::Result<()> {
        if self.restored {
            return Ok(());
        }
        if unsafe {
            windows_sys::Win32::System::StationsAndDesktops::SetThreadDesktop(self.original)
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        self.restored = true;
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for DesktopThreadAttachment {
    fn drop(&mut self) {
        if !self.restored {
            unsafe {
                windows_sys::Win32::System::StationsAndDesktops::SetThreadDesktop(self.original)
            };
        }
    }
}

#[cfg(windows)]
pub struct IsolatedProcess {
    process: std::os::windows::io::OwnedHandle,
    process_id: u32,
}

#[cfg(windows)]
impl IsolatedProcess {
    pub fn spawn_suspended(
        target: &[String],
        environment_overrides: &[(std::ffi::OsString, std::ffi::OsString)],
        desktop: &str,
    ) -> std::io::Result<Self> {
        use std::{os::windows::io::FromRawHandle, ptr};
        use windows_sys::Win32::System::Threading::{
            CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, PROCESS_INFORMATION,
            STARTUPINFOW,
        };

        let mut command_line = windows_command_line(target)?;
        let environment = windows_environment(environment_overrides)?;
        let mut desktop = desktop.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        let startup = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            lpDesktop: desktop.as_mut_ptr(),
            ..Default::default()
        };
        let mut process_info = PROCESS_INFORMATION::default();
        if unsafe {
            CreateProcessW(
                ptr::null(),
                command_line.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                0,
                CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
                environment.as_ptr().cast(),
                ptr::null(),
                &raw const startup,
                &raw mut process_info,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        let process =
            unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(process_info.hProcess) };
        let initial_thread =
            unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(process_info.hThread) };
        drop(initial_thread);
        Ok(Self {
            process,
            process_id: process_info.dwProcessId,
        })
    }

    pub const fn id(&self) -> u32 {
        self.process_id
    }

    pub fn raw_handle(&self) -> windows_sys::Win32::Foundation::HANDLE {
        use std::os::windows::io::AsRawHandle;
        self.process.as_raw_handle()
    }

    pub fn kill(&mut self) -> std::io::Result<()> {
        if unsafe { windows_sys::Win32::System::Threading::TerminateProcess(self.raw_handle(), 1) }
            == 0
        {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn try_wait(&mut self) -> std::io::Result<Option<()>> {
        use windows_sys::Win32::{
            Foundation::{WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT},
            System::Threading::WaitForSingleObject,
        };
        match unsafe { WaitForSingleObject(self.raw_handle(), 0) } {
            WAIT_OBJECT_0 => Ok(Some(())),
            WAIT_TIMEOUT => Ok(None),
            WAIT_FAILED => Err(std::io::Error::last_os_error()),
            result => Err(std::io::Error::other(format!(
                "unexpected process wait result {result}"
            ))),
        }
    }

    pub fn wait(&mut self) -> std::io::Result<()> {
        use windows_sys::Win32::{
            Foundation::{WAIT_FAILED, WAIT_OBJECT_0},
            System::Threading::{INFINITE, WaitForSingleObject},
        };
        match unsafe { WaitForSingleObject(self.raw_handle(), INFINITE) } {
            WAIT_OBJECT_0 => Ok(()),
            WAIT_FAILED => Err(std::io::Error::last_os_error()),
            result => Err(std::io::Error::other(format!(
                "unexpected process wait result {result}"
            ))),
        }
    }
}

#[cfg(windows)]
fn append_windows_argument(
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

#[cfg(windows)]
fn windows_command_line(target: &[String]) -> std::io::Result<Vec<u16>> {
    let executable = target.first().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "target command is empty")
    })?;
    let mut command_line = Vec::new();
    append_windows_argument(&mut command_line, std::ffi::OsStr::new(executable), true)?;
    for argument in &target[1..] {
        command_line.push(b' ' as u16);
        append_windows_argument(&mut command_line, std::ffi::OsStr::new(argument), false)?;
    }
    command_line.push(0);
    Ok(command_line)
}

#[cfg(windows)]
fn windows_environment(
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

#[cfg(all(test, windows))]
mod desktop_tests {
    use super::*;

    #[test]
    fn private_desktop_attaches_and_restores() -> std::io::Result<()> {
        use windows_sys::Win32::System::{
            StationsAndDesktops::GetThreadDesktop, Threading::GetCurrentThreadId,
        };

        let original = unsafe { GetThreadDesktop(GetCurrentThreadId()) };
        let name = format!(
            "snowglass-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let (desktop, mut attachment) = PrivateDesktop::create_and_attach_current_thread(&name)?;
        assert_eq!(
            unsafe { GetThreadDesktop(GetCurrentThreadId()) },
            desktop.handle
        );
        attachment.restore()?;
        assert_eq!(unsafe { GetThreadDesktop(GetCurrentThreadId()) }, original);
        Ok(())
    }
}

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

#[cfg(windows)]
pub fn process_architecture(
    process: windows_sys::Win32::Foundation::HANDLE,
) -> std::io::Result<ProcessArchitecture> {
    use windows_sys::Win32::System::Threading::IsWow64Process2;

    const IMAGE_FILE_MACHINE_UNKNOWN: u16 = 0;
    const IMAGE_FILE_MACHINE_AMD64: u16 = 0x8664;
    const IMAGE_FILE_MACHINE_ARM64: u16 = 0xaa64;

    let mut process_machine = 0;
    let mut native_machine = 0;
    if unsafe { IsWow64Process2(process, &mut process_machine, &mut native_machine) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let machine = if process_machine == IMAGE_FILE_MACHINE_UNKNOWN {
        native_machine
    } else {
        process_machine
    };
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
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
    };

    let initial_thread = first_process_thread(process_id)?;
    let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, initial_thread) };
    if thread.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    let resume = unsafe { ResumeThread(thread) };
    unsafe { CloseHandle(thread) };
    if resume == u32::MAX {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
fn first_process_thread(process_id: ProcessId) -> std::io::Result<u32> {
    use std::mem;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
        System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        },
    };
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    let mut entry = THREADENTRY32 {
        dwSize: mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut found = None;
    if unsafe { Thread32First(snapshot, &mut entry) } != 0 {
        loop {
            if entry.th32OwnerProcessID == process_id.0 {
                found = Some(entry.th32ThreadID);
                break;
            }
            if unsafe { Thread32Next(snapshot, &mut entry) } == 0 {
                break;
            }
        }
    }
    unsafe { CloseHandle(snapshot) };
    found
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "target thread not found"))
}

#[cfg(windows)]
pub struct WindowKeyForwarder {
    viewer: windows_sys::Win32::Foundation::HWND,
    context: *mut std::sync::atomic::AtomicIsize,
}

#[cfg(windows)]
impl WindowKeyForwarder {
    pub fn install(
        viewer: NativeWindow,
        backing_window: std::sync::Arc<std::sync::atomic::AtomicIsize>,
    ) -> std::io::Result<Self> {
        use windows_sys::Win32::UI::Shell::SetWindowSubclass;
        let context =
            std::sync::Arc::into_raw(backing_window) as *mut std::sync::atomic::AtomicIsize;
        let viewer = viewer.0 as windows_sys::Win32::Foundation::HWND;
        // SAFETY: context remains allocated until Drop removes this exact
        // subclass, and the viewer HWND is live while the forwarder is held.
        if unsafe {
            SetWindowSubclass(
                viewer,
                Some(key_forward_subclass),
                context as usize,
                context as usize,
            )
        } == 0
        {
            // SAFETY: installation failed, so no callback owns this Arc.
            unsafe { drop(std::sync::Arc::from_raw(context)) };
            Err(std::io::Error::last_os_error())
        } else {
            Ok(Self { viewer, context })
        }
    }
}

#[cfg(windows)]
impl Drop for WindowKeyForwarder {
    fn drop(&mut self) {
        use windows_sys::Win32::UI::Shell::RemoveWindowSubclass;
        // SAFETY: this removes the callback before releasing its context.
        unsafe {
            RemoveWindowSubclass(
                self.viewer,
                Some(key_forward_subclass),
                self.context as usize,
            );
            drop(std::sync::Arc::from_raw(self.context));
        }
    }
}

#[cfg(windows)]
unsafe extern "system" fn key_forward_subclass(
    window: windows_sys::Win32::Foundation::HWND,
    message: u32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
    _subclass_id: usize,
    context: usize,
) -> windows_sys::Win32::Foundation::LRESULT {
    use std::sync::atomic::Ordering;
    use windows_sys::Win32::UI::{
        Shell::DefSubclassProc,
        WindowsAndMessaging::{
            PostMessageW, SIZE_MINIMIZED, WM_CHAR, WM_DEADCHAR, WM_KEYDOWN, WM_KEYUP, WM_SIZE,
            WM_SYSCHAR, WM_SYSDEADCHAR, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_UNICHAR,
        },
    };
    // SAFETY: SetWindowSubclass supplies the live Arc allocation above.
    let target =
        unsafe { &*(context as *const std::sync::atomic::AtomicIsize) }.load(Ordering::Acquire);

    // Resizing the foreground viewer deactivates the hidden source window. Restore
    // its logical focus without showing it or changing its size.
    if message == WM_SIZE && wparam != SIZE_MINIMIZED as usize && target != 0 {
        unsafe { post_logical_focus(target) };
    }
    if matches!(
        message,
        WM_KEYDOWN
            | WM_KEYUP
            | WM_SYSKEYDOWN
            | WM_SYSKEYUP
            | WM_CHAR
            | WM_DEADCHAR
            | WM_SYSCHAR
            | WM_SYSDEADCHAR
            | WM_UNICHAR
    ) {
        if target != 0 {
            if matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN) {
                unsafe { post_logical_focus(target) };
            }
            unsafe { PostMessageW(target as _, message, wparam, lparam) };
        }
    }
    unsafe { DefSubclassProc(window, message, wparam, lparam) }
}

#[cfg(windows)]
unsafe fn post_logical_focus(target: isize) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        PostMessageW, WA_ACTIVE, WM_ACTIVATE, WM_ACTIVATEAPP, WM_SETFOCUS,
    };
    unsafe {
        PostMessageW(target as _, WM_ACTIVATEAPP, 1, 0);
        PostMessageW(target as _, WM_ACTIVATE, WA_ACTIVE as usize, 0);
        PostMessageW(target as _, WM_SETFOCUS, 0, 0);
    }
}

#[cfg(windows)]
pub fn forward_focus(window: NativeWindow, focused: bool) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        PostMessageW, WA_INACTIVE, WM_ACTIVATE, WM_ACTIVATEAPP, WM_KILLFOCUS,
    };
    unsafe {
        if focused {
            post_logical_focus(window.0);
        } else {
            PostMessageW(window.0 as _, WM_KILLFOCUS, 0, 0);
            PostMessageW(window.0 as _, WM_ACTIVATE, WA_INACTIVE as usize, 0);
            PostMessageW(window.0 as _, WM_ACTIVATEAPP, 0, 0);
        }
    }
}

#[cfg(windows)]
pub fn performance_counter() -> std::io::Result<u64> {
    let mut value = 0i64;
    // SAFETY: the pointer refers to writable storage for the duration of the call.
    if unsafe { windows_sys::Win32::System::Performance::QueryPerformanceCounter(&mut value) } == 0
    {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(value as u64)
    }
}

#[cfg(windows)]
pub fn performance_frequency() -> std::io::Result<u64> {
    let mut value = 0i64;
    // SAFETY: the pointer refers to writable storage for the duration of the call.
    if unsafe { windows_sys::Win32::System::Performance::QueryPerformanceFrequency(&mut value) }
        == 0
    {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(value as u64)
    }
}

#[cfg(windows)]
pub fn duplicate_handle_into_process(
    source_handle: isize,
    target_process_id: ProcessId,
) -> std::io::Result<isize> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE},
        System::Threading::{GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE},
    };

    // SAFETY: handles are checked for null, the target process handle is closed
    // on every path, and DuplicateHandle initializes `duplicated` on success.
    unsafe {
        let target_process = OpenProcess(PROCESS_DUP_HANDLE, 0, target_process_id.0);
        if target_process.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let mut duplicated: HANDLE = std::ptr::null_mut();
        let result = DuplicateHandle(
            GetCurrentProcess(),
            source_handle as HANDLE,
            target_process,
            &mut duplicated,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        );
        let error = (result == 0).then(std::io::Error::last_os_error);
        CloseHandle(target_process);
        if let Some(error) = error {
            Err(error)
        } else {
            Ok(duplicated as isize)
        }
    }
}

#[cfg(windows)]
pub fn find_main_window(process_id: ProcessId) -> Option<NativeWindow> {
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM, RECT},
        UI::WindowsAndMessaging::{EnumWindows, GetClientRect, GetWindowThreadProcessId},
    };

    struct Context {
        process_id: u32,
        best: HWND,
        score: i64,
    }
    unsafe extern "system" fn callback(window: HWND, parameter: LPARAM) -> i32 {
        let context = unsafe { &mut *(parameter as *mut Context) };
        let mut owner = 0;
        unsafe { GetWindowThreadProcessId(window, &mut owner) };
        if owner == context.process_id {
            let mut rect = RECT::default();
            if unsafe { GetClientRect(window, &mut rect) } != 0 {
                let area = i64::from(rect.right - rect.left) * i64::from(rect.bottom - rect.top);
                let style = unsafe {
                    windows_sys::Win32::UI::WindowsAndMessaging::GetWindowLongW(
                        window,
                        windows_sys::Win32::UI::WindowsAndMessaging::GWL_STYLE,
                    )
                } as u32;
                let borderless =
                    style & windows_sys::Win32::UI::WindowsAndMessaging::WS_CAPTION == 0;
                let score = area + if borderless { 1i64 << 60 } else { 0 };
                if score > context.score {
                    context.best = window;
                    context.score = score;
                }
            }
        }
        1
    }
    let mut context = Context {
        process_id: process_id.0,
        best: std::ptr::null_mut(),
        score: 0,
    };
    unsafe { EnumWindows(Some(callback), (&mut context as *mut Context) as LPARAM) };
    (!context.best.is_null()).then_some(NativeWindow(context.best as isize))
}

#[cfg(windows)]
pub fn is_borderless_window(window: NativeWindow) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GWL_STYLE, GetWindowLongW, WS_CAPTION};
    let style = unsafe { GetWindowLongW(window.0 as _, GWL_STYLE) } as u32;
    style & WS_CAPTION == 0
}

#[cfg(windows)]
pub fn client_size(window: NativeWindow) -> Option<(u32, u32)> {
    let mut rect = windows_sys::Win32::Foundation::RECT::default();
    (unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(window.0 as _, &mut rect)
    } != 0)
        .then_some((
            (rect.right - rect.left).max(0) as u32,
            (rect.bottom - rect.top).max(0) as u32,
        ))
}

#[cfg(windows)]
pub fn is_process_window(window: NativeWindow, process_id: ProcessId) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowThreadProcessId, IsWindow};
    let hwnd = window.0 as windows_sys::Win32::Foundation::HWND;
    if unsafe { IsWindow(hwnd) } == 0 {
        return false;
    }
    let mut owner = 0;
    unsafe { GetWindowThreadProcessId(hwnd, &mut owner) };
    owner == process_id.0
}

#[cfg(windows)]
pub fn resize_client(window: NativeWindow, width: u32, height: u32) -> std::io::Result<()> {
    use windows_sys::Win32::{
        Foundation::{HWND, RECT},
        UI::WindowsAndMessaging::{
            AdjustWindowRectEx, GWL_EXSTYLE, GWL_STYLE, GetWindowLongW, SWP_HIDEWINDOW, SWP_NOMOVE,
            SWP_NOZORDER, SetWindowPos,
        },
    };
    if width == 0 || height == 0 {
        return Ok(());
    }
    let hwnd = window.0 as HWND;
    let mut client = RECT::default();
    if unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut client) } != 0
        && (client.right - width as i32).abs() <= 2
        && (client.bottom - height as i32).abs() <= 2
    {
        return Ok(());
    }
    let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) } as u32;
    let ex_style = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32;
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: width as i32,
        bottom: height as i32,
    };
    if unsafe { AdjustWindowRectEx(&mut rect, style, 0, ex_style) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe {
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            0,
            0,
            rect.right - rect.left,
            rect.bottom - rect.top,
            SWP_NOMOVE | SWP_NOZORDER | SWP_HIDEWINDOW,
        )
    } == 0
    {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
pub fn forward_key(window: NativeWindow, virtual_key: u32, pressed: bool, repeat: bool) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{MAPVK_VK_TO_VSC_EX, MapVirtualKeyW};
    use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_KEYDOWN, WM_KEYUP};
    let message = if pressed { WM_KEYDOWN } else { WM_KEYUP };
    let mapped = unsafe { MapVirtualKeyW(virtual_key, MAPVK_VK_TO_VSC_EX) };
    let scan_code = mapped & 0xff;
    let prefix = (mapped >> 8) & 0xff;
    let mut lparam = 1isize | ((scan_code as isize) << 16);
    if prefix == 0xe0 || prefix == 0xe1 {
        lparam |= 1 << 24;
    }
    if repeat || !pressed {
        lparam |= 1 << 30;
    }
    if !pressed {
        lparam |= 1 << 31;
    }
    unsafe { PostMessageW(window.0 as _, message, virtual_key as usize, lparam) };
}

/// Forwards a physical keyboard transition without losing the scan-code prefix
/// supplied by Raw Input. This is important for navigation keys, whose E0
/// prefix distinguishes them from their numeric-keypad counterparts.
#[cfg(windows)]
pub fn forward_raw_key(
    window: NativeWindow,
    virtual_key: u32,
    scan_code: u16,
    extended: bool,
    pressed: bool,
    repeat: bool,
) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_KEYDOWN, WM_KEYUP};
    let message = if pressed { WM_KEYDOWN } else { WM_KEYUP };
    let mut lparam = 1isize | (((scan_code & 0xff) as isize) << 16);
    if extended {
        lparam |= 1 << 24;
    }
    if repeat || !pressed {
        lparam |= 1 << 30;
    }
    if !pressed {
        lparam |= 1 << 31;
    }
    unsafe { PostMessageW(window.0 as _, message, virtual_key as usize, lparam) };
}

#[cfg(windows)]
pub fn forward_character(window: NativeWindow, character: char) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CHAR};
    let mut units = [0; 2];
    for unit in character.encode_utf16(&mut units) {
        unsafe { PostMessageW(window.0 as _, WM_CHAR, *unit as usize, 1) };
    }
}

#[cfg(windows)]
pub fn forward_mouse_move(window: NativeWindow, x: i32, y: i32) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_MOUSEMOVE};
    let lparam = ((y as u16 as u32) << 16 | x as u16 as u32) as isize;
    unsafe { PostMessageW(window.0 as _, WM_MOUSEMOVE, 0, lparam) };
}

#[cfg(windows)]
pub fn forward_mouse_button(window: NativeWindow, button: u16, pressed: bool, x: i32, y: i32) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        PostMessageW, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_RBUTTONDOWN,
        WM_RBUTTONUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
    };
    let message = match (button, pressed) {
        (1, true) => WM_LBUTTONDOWN,
        (1, false) => WM_LBUTTONUP,
        (2, true) => WM_RBUTTONDOWN,
        (2, false) => WM_RBUTTONUP,
        (3, true) => WM_MBUTTONDOWN,
        (3, false) => WM_MBUTTONUP,
        (_, true) => WM_XBUTTONDOWN,
        (_, false) => WM_XBUTTONUP,
    };
    let wparam = if button >= 4 {
        ((button - 3) as usize) << 16
    } else {
        0
    };
    let lparam = ((y as u16 as u32) << 16 | x as u16 as u32) as isize;
    unsafe { PostMessageW(window.0 as _, message, wparam, lparam) };
}

#[cfg(windows)]
pub fn forward_mouse_wheel(window: NativeWindow, delta: i16, horizontal: bool) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        PostMessageW, WM_MOUSEHWHEEL, WM_MOUSEWHEEL,
    };
    let message = if horizontal {
        WM_MOUSEHWHEEL
    } else {
        WM_MOUSEWHEEL
    };
    unsafe { PostMessageW(window.0 as _, message, (delta as u16 as usize) << 16, 0) };
}
