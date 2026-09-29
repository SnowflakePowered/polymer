use super::*;
use polymer_hook::module;
use std::os::windows::ffi::OsStrExt;
use windows_sys::Win32::{Foundation::HMODULE, System::LibraryLoader::LoadLibraryW};

pub(crate) struct Sdl3ExportState {
    table: Sdl3TableState,
}

impl Default for Sdl3ExportState {
    fn default() -> Self {
        let table = create_table_state().expect("the SDL3 export hook state is valid");
        if let Ok(session) = config::session() {
            table.runtime.start_receiver(format!("{session}-input"));
        }
        Self { table }
    }
}

const SDL3_DLL: &[u16] = &[
    b'S' as u16,
    b'D' as u16,
    b'L' as u16,
    b'3' as u16,
    b'.' as u16,
    b'd' as u16,
    b'l' as u16,
    b'l' as u16,
    0,
];

#[unsafe(no_mangle)]
/// Installs the SDL3 virtual-gamepad hooks into the current process.
///
/// # Safety
///
/// The caller must invoke this only after the DLL has been loaded into a
/// Windows process whose module list and dynamic API table are valid.
pub unsafe extern "system" fn PolymerInstallSdl3Hook() -> i32 {
    polymer_hook::telemetry::init_file_tracing(config::log_file(config::SDL_LOG_FILE));
    discover_loaded_module();
    0
}

fn dynamic_api_module() -> Option<HMODULE> {
    let path = std::env::var_os("SDL3_DYNAMIC_API")?;
    let mut wide = path.encode_wide().collect::<Vec<_>>();
    wide.push(0);
    let module = unsafe { LoadLibraryW(wide.as_ptr()) };
    if module.is_null() {
        tracing::debug!("failed to preload SDL3_DYNAMIC_API runtime");
        None
    } else {
        Some(module)
    }
}

fn discover_loaded_module() {
    if let Some(module) = dynamic_api_module().or_else(|| module::loaded(SDL3_DLL))
        && (unsafe { api::symbol::<api::DynapiEntryFn>(module, c"SDL_DYNAPI_entry") }).is_some()
    {
        install();
    }
}

fn install() {
    let dynamic_module = dynamic_api_module().or_else(|| module::loaded(SDL3_DLL));
    let frontend_module = module::loaded(SDL3_DLL);
    if dynamic_module.is_some() && frontend_module.is_some() && dynamic_module != frontend_module {
        if Sdl3ExportInstallation::install().is_err() {
            tracing::debug!("failed to enable SDL3 frontend export detours");
            return;
        }
        tracing::debug!("installed SDL3 frontend export gamepad hooks");
    } else {
        if Sdl3EntryInstallation::install().is_err() {
            tracing::debug!("failed to enable SDL3 dynamic API detour");
            return;
        }
        tracing::debug!("installed SDL3 dynamic API gamepad hooks");
    }
}

fn prepare_export_gamepads(state: &Sdl3ExportState) {
    if prepare_enumeration(&state.table)
        && !state.table.logged_enumeration.swap(true, Ordering::AcqRel)
    {
        let count = state.table.runtime.topology_pad_count();
        tracing::debug!("enumerated {count} SDL3 gamepad(s) through frontend exports");
    }
}

type InitSubSystemFn = unsafe extern "C" fn(u32) -> bool;
type GetGamepadAxisFn = unsafe extern "C" fn(*mut c_void, i32) -> i16;
type GetGamepadButtonFn = unsafe extern "C" fn(*mut c_void, i32) -> bool;
type GetJoystickAxisFn = unsafe extern "C" fn(*mut c_void, i32) -> i16;
type GetJoystickButtonFn = unsafe extern "C" fn(*mut c_void, i32) -> bool;
type PollEventFn = unsafe extern "C" fn(*mut c_void) -> bool;

fn frontend_symbol<T: Copy>(name: &std::ffi::CStr) -> T {
    let module = module::loaded(SDL3_DLL).expect("the SDL3 frontend module is loaded");
    unsafe { api::symbol(module, name) }.expect("the SDL3 frontend export is available")
}

fn init_subsystem_target() -> InitSubSystemFn {
    frontend_symbol(c"SDL_InitSubSystem")
}

fn get_gamepad_axis_target() -> GetGamepadAxisFn {
    frontend_symbol(c"SDL_GetGamepadAxis")
}

fn get_gamepad_button_target() -> GetGamepadButtonFn {
    frontend_symbol(c"SDL_GetGamepadButton")
}

fn get_joystick_axis_target() -> GetJoystickAxisFn {
    frontend_symbol(c"SDL_GetJoystickAxis")
}

fn get_joystick_button_target() -> GetJoystickButtonFn {
    frontend_symbol(c"SDL_GetJoystickButton")
}

fn poll_event_target() -> PollEventFn {
    frontend_symbol(c"SDL_PollEvent")
}

polymer_hook::chain_hooks! {
    #![order(reverse)]
    #![detour(polymer_platform_windows::InlineDetour<_>)]
    #[installer(state = Sdl3ExportState)]
    pub(super) struct Sdl3ExportInstallation;

    #[trampoline]
    #[terminal(installed(target = init_subsystem_target()), fallback = false)]
    #[postfix(|result, next| {
        if result {
            prepare_export_gamepads(next.state.as_ref());
        }
    })]
    unsafe extern "C" fn init_subsystem(flags: u32) -> bool;

    #[trampoline]
    #[terminal(installed(target = get_gamepad_axis_target()), fallback = 0)]
    #[prefix(|next| flush_gamepads(&next.state.table))]
    unsafe extern "C" fn exported_get_gamepad_axis(
        gamepad: *mut c_void,
        axis: i32,
    ) -> i16;

    #[trampoline]
    #[terminal(installed(target = get_gamepad_button_target()), fallback = false)]
    #[prefix(|next| flush_gamepads(&next.state.table))]
    unsafe extern "C" fn exported_get_gamepad_button(
        gamepad: *mut c_void,
        button: i32,
    ) -> bool;

    #[trampoline]
    #[terminal(installed(target = get_joystick_axis_target()), fallback = 0)]
    #[prefix(|next| flush_gamepads(&next.state.table))]
    unsafe extern "C" fn exported_get_joystick_axis(
        joystick: *mut c_void,
        axis: i32,
    ) -> i16;

    #[trampoline]
    #[terminal(installed(target = get_joystick_button_target()), fallback = false)]
    #[prefix(|next| flush_gamepads(&next.state.table))]
    unsafe extern "C" fn exported_get_joystick_button(
        joystick: *mut c_void,
        button: i32,
    ) -> bool;

    #[trampoline]
    #[terminal(installed(target = poll_event_target()), fallback = false)]
    #[prefix(|next| {
        prepare_export_gamepads(next.state.as_ref());
        flush_gamepads(&next.state.table);
    })]
    unsafe extern "C" fn exported_poll_event(event: *mut c_void) -> bool;
}

pub(super) fn backend_module() -> Option<HMODULE> {
    dynamic_api_module().or_else(|| module::loaded(SDL3_DLL))
}
