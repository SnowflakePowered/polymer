use crate::runtime::Runtime;
use windows_sys::Win32::{
    Foundation::ERROR_DEVICE_NOT_CONNECTED,
    UI::Input::XboxController::{XINPUT_STATE, XINPUT_VIBRATION},
};

polymer_hook::chain_hooks! {
    #![order(reverse)]
    #![detour(polymer_platform_windows::InlineDetour<_>)]
    #[installer(state = crate::runtime::Runtime)]
    pub(crate) struct HookInstallation;

    #[trampoline]
    #[terminal(installed(target = Runtime::target(|state| state.get_state)), fallback = ERROR_DEVICE_NOT_CONNECTED)]
    #[infix(crate::runtime::Runtime::get_state)]
    pub unsafe extern "system" fn xinput_get_state(user: u32, state: *mut XINPUT_STATE) -> u32;

    #[trampoline]
    #[terminal(installed(target = Runtime::target(|state| state.get_state_ex)), fallback = ERROR_DEVICE_NOT_CONNECTED)]
    #[infix(crate::runtime::Runtime::get_state_ex)]
    pub unsafe extern "system" fn xinput_get_state_ex(user: u32, state: *mut XINPUT_STATE) -> u32;

    #[trampoline]
    #[terminal(installed(target = Runtime::target(|state| state.set_state)), fallback = ERROR_DEVICE_NOT_CONNECTED)]
    #[infix(crate::runtime::Runtime::set_state)]
    pub unsafe extern "system" fn xinput_set_state(user: u32, vibration: *const XINPUT_VIBRATION) -> u32;
}
