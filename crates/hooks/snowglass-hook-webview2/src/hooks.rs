//! Process-creation hook declarations.

use windows_sys::Win32::{
    Security::SECURITY_ATTRIBUTES,
    System::Threading::{PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, STARTUPINFOW},
};
use windows_sys::core::{BOOL, PCWSTR, PWSTR};

snowglass_hook::chain_hooks! {
    #![order(reverse)]
    #![detour(snowglass_platform_windows::InlineDetour<_>)]
    #[installer]
    pub(crate) struct HookInstallation;

    #[trampoline]
    #[terminal(installed(
        target = windows_sys::Win32::System::Threading::CreateProcessW,
    ), fallback = 0)]
    #[infix(crate::runtime::create_process)]
    pub unsafe extern "system" fn create_process(
        application_name: PCWSTR,
        command_line: PWSTR,
        process_attributes: *const SECURITY_ATTRIBUTES,
        thread_attributes: *const SECURITY_ATTRIBUTES,
        inherit_handles: BOOL,
        creation_flags: PROCESS_CREATION_FLAGS,
        environment: *const core::ffi::c_void,
        current_directory: PCWSTR,
        startup_info: *const STARTUPINFOW,
        process_information: *mut PROCESS_INFORMATION,
    ) -> BOOL;
}
