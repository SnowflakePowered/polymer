use std::ffi::c_void;

snowglass_hook::chain_hooks! {
    #![order(reverse)]
    #![detour(snowglass_platform_windows::InlineDetour<_>)]
    #[installer]
    pub(crate) struct GtkHookInstallation;

    #[trampoline(abi = "C")]
    #[terminal(installed(target = super::runtime::dialog_run_target()), fallback = -1)]
    #[infix(super::runtime::dialog_run)]
    pub(super) unsafe extern "C" fn gtk_dialog_run(dialog: *mut c_void) -> i32;
}
