use std::ffi::c_void;

snowglass_hook::chain_hooks! {
    #![order(reverse)]
    #![detour(snowglass_platform_windows::InlineDetour<_>)]
    #[installer]
    pub(crate) struct QtHookInstallation;

    #[trampoline(abi = "C")]
    #[terminal(installed(target = super::runtime::dialog_exec_target()), fallback = 0)]
    #[infix(super::runtime::dialog_exec)]
    pub(super) unsafe extern "C" fn qt_dialog_exec(dialog: *mut c_void) -> i32;
}
