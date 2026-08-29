//! SDL3 module discovery and detour ownership.

use crate::api;
use parking_lot::Mutex;
use snowglass_hook::config;
use snowglass_input::layout::InputLayout;
use snowglass_input::projection::Projection;
use snowglass_input_sdl::Runtime;
use std::{
    ffi::c_void,
    os::windows::ffi::OsStrExt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use windows_sys::Win32::{
    Foundation::HMODULE,
    System::LibraryLoader::{GetModuleHandleW, LoadLibraryW},
};

pub(crate) struct Sdl3TableState {
    backend: Mutex<api::Api>,
    runtime: Arc<Runtime>,
    logged_enumeration: AtomicBool,
    logged_gamepad_consumer: AtomicBool,
    logged_gamepad_update: AtomicBool,
}

pub(crate) struct Sdl3EntryState {
    target: api::DynapiEntryFn,
}

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

impl Default for Sdl3EntryState {
    fn default() -> Self {
        let module = dynamic_api_module()
            .or_else(|| loaded_module(SDL3_DLL))
            .expect("SDL3 module was resolved before entry-hook installation");
        let target = unsafe { api::symbol(module, c"SDL_DYNAPI_entry") }
            .expect("SDL3 dynamic API entry target was resolved before installation");
        Self { target }
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

// SDL3's ABI-stable dynamic API table is append-only. These slots come from
// SDL_dynapi_procs.h in the runtime staged with this hook.
const DYNAPI_GET_GAMEPADS: usize = 349;
const DYNAPI_GET_GAMEPAD_AXIS: usize = 306;
const DYNAPI_GET_GAMEPAD_BUTTON: usize = 309;
const DYNAPI_GET_JOYSTICKS: usize = 395;
const DYNAPI_POLL_EVENT: usize = 677;
const DYNAPI_PUMP_EVENTS: usize = 681;
const DYNAPI_UPDATE_GAMEPADS: usize = 952;
const DYNAPI_UPDATE_JOYSTICKS: usize = 954;
const DYNAPI_WAIT_EVENT: usize = 972;
const DYNAPI_WAIT_EVENT_TIMEOUT: usize = 973;

fn dynapi_entry_target() -> api::DynapiEntryFn {
    Sdl3EntryInstallation::with_state(|state| state.target)
        .expect("SDL3 entry-hook state was installed before target resolution")
}

fn create_table_state() -> Result<Sdl3TableState, &'static str> {
    let module = dynamic_api_module()
        .or_else(|| loaded_module(SDL3_DLL))
        .ok_or("SDL3 module is not loaded")?;
    let projection = Projection::embedded().map_err(|_| "the SDL projection is invalid")?;
    let layouts = config::layouts()
        .into_iter()
        .map(|layout_id| {
            InputLayout::get(&layout_id)
                .ok_or("the requested input layout is unavailable")
                .map(|layout| {
                    (
                        layout.sdl_output(&projection),
                        "Snowglass Virtual Gamepad".to_owned(),
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if layouts.len() != config::sdl_gamepad_count() {
        return Err("input layout count disagrees with virtual gamepad count");
    }
    let runtime = Arc::new(Runtime::new(config::sdl_gamepad_count()));
    let backend = unsafe { api::Api::load(module, layouts, runtime.clone()) }
        .ok_or("SDL3 virtual joystick exports are unavailable")?;
    Ok(Sdl3TableState {
        backend: Mutex::new(backend),
        runtime,
        logged_enumeration: AtomicBool::new(false),
        logged_gamepad_consumer: AtomicBool::new(false),
        logged_gamepad_update: AtomicBool::new(false),
    })
}

type InitSubSystemFn = unsafe extern "C" fn(u32) -> bool;
type GetGamepadAxisFn = unsafe extern "C" fn(*mut c_void, i32) -> i16;
type GetGamepadButtonFn = unsafe extern "C" fn(*mut c_void, i32) -> bool;
type GetJoystickAxisFn = unsafe extern "C" fn(*mut c_void, i32) -> i16;
type GetJoystickButtonFn = unsafe extern "C" fn(*mut c_void, i32) -> bool;
type PollEventFn = unsafe extern "C" fn(*mut c_void) -> bool;

fn frontend_symbol<T: Copy>(name: &std::ffi::CStr) -> T {
    let module = loaded_module(SDL3_DLL).expect("the SDL3 frontend module is loaded");
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

snowglass_hook::chain_hooks! {
    #![order(reverse)]
    #![detour(snowglass_platform_windows::InlineDetour<_>)]
    #[installer(state = Sdl3EntryState)]
    pub(crate) struct Sdl3EntryInstallation;

    #[trampoline]
    #[terminal(installed(target = dynapi_entry_target()), fallback = -1)]
    #[postfix(captured_dynapi_table(table, tablesize, result))]
    unsafe extern "C" fn dynapi_entry(
        apiver: u32,
        table: *mut c_void,
        tablesize: u32,
    ) -> i32;
}

snowglass_hook::chain_hooks! {
    #![order(reverse)]
    #![detour(snowglass_platform_windows::InlineDetour<_>)]
    #[installer(state = Sdl3ExportState)]
    pub(crate) struct Sdl3ExportInstallation;

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

snowglass_hook::function_table_hook! {
    #[state: Sdl3TableState = create_table_state()?]
    pub(crate) struct Sdl3DynapiHook {
        const MINIMUM_SLOTS: usize = DYNAPI_WAIT_EVENT_TIMEOUT + 1;

        #[slot = DYNAPI_GET_GAMEPAD_AXIS]
        #[prefix(|next| flush_gamepads(
            next.state,
        ))]
        unsafe extern "C" fn get_gamepad_axis_dispatch(
            gamepad: *mut c_void,
            axis: i32,
        ) -> i16;

        #[slot = DYNAPI_GET_GAMEPAD_BUTTON]
        #[prefix(|next| flush_gamepads(
            next.state,
        ))]
        unsafe extern "C" fn get_gamepad_button_dispatch(
            gamepad: *mut c_void,
            button: i32,
        ) -> bool;

        #[slot = DYNAPI_GET_GAMEPADS]
        #[infix(enumerate_gamepads)]
        unsafe extern "C" fn get_gamepads_dispatch(
            count: *mut i32,
        ) -> *mut api::JoystickId;

        #[slot = DYNAPI_GET_JOYSTICKS]
        #[infix(enumerate_joysticks)]
        unsafe extern "C" fn get_joysticks_dispatch(
            count: *mut i32,
        ) -> *mut api::JoystickId;

        #[slot = DYNAPI_POLL_EVENT]
        #[prefix(|next| flush_gamepads(
            next.state,
        ))]
        unsafe extern "C" fn poll_event_dispatch(
            event: *mut c_void,
        ) -> bool;

        #[slot = DYNAPI_PUMP_EVENTS]
        #[prefix(|next| flush_gamepads(
            next.state,
        ))]
        unsafe extern "C" fn pump_events_dispatch();

        #[slot = DYNAPI_UPDATE_JOYSTICKS]
        #[prefix(|next| flush_gamepads(
            next.state,
        ))]
        unsafe extern "C" fn update_joysticks_dispatch();

        #[slot = DYNAPI_UPDATE_GAMEPADS]
        #[prefix(|next| flush_gamepads(
            next.state,
        ))]
        unsafe extern "C" fn update_gamepads_dispatch();

        #[slot = DYNAPI_WAIT_EVENT]
        #[prefix(|next| flush_gamepads(
            next.state,
        ))]
        unsafe extern "C" fn wait_event_dispatch(
            event: *mut c_void,
        ) -> bool;

        #[slot = DYNAPI_WAIT_EVENT_TIMEOUT]
        #[prefix(|next| flush_gamepads(
            next.state,
        ))]
        unsafe extern "C" fn wait_event_timeout_dispatch(
            event: *mut c_void,
            timeout_ms: i32,
        ) -> bool;
    }
}

#[unsafe(no_mangle)]
/// Installs the SDL3 virtual-gamepad hooks into the current process.
///
/// # Safety
///
/// The caller must invoke this only after the DLL has been loaded into a
/// Windows process whose module list and dynamic API table are valid.
pub unsafe extern "system" fn SnowglassInstallSdl3Hook() -> i32 {
    snowglass_hook::telemetry::init_file_tracing(config::sdl_log_file());
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
    if let Some(module) = dynamic_api_module().or_else(|| loaded_module(SDL3_DLL))
        && (unsafe { api::symbol::<api::DynapiEntryFn>(module, c"SDL_DYNAPI_entry") }).is_some()
    {
        install();
    }
}

fn loaded_module(name: &[u16]) -> Option<HMODULE> {
    let module = unsafe { GetModuleHandleW(name.as_ptr()) };
    (!module.is_null()).then_some(module)
}

fn install() {
    let dynamic_module = dynamic_api_module().or_else(|| loaded_module(SDL3_DLL));
    let frontend_module = loaded_module(SDL3_DLL);
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

unsafe fn captured_dynapi_table(
    table: *mut c_void,
    tablesize: u32,
    result: i32,
    _context: &DynapiEntryContext<'_>,
) {
    if result < 0 || table.is_null() {
        return;
    }

    let state = match unsafe { Sdl3DynapiHook::install(table, tablesize) } {
        Ok(state) => state,
        Err(error) => {
            tracing::debug!(message = %(error));
            return;
        }
    };
    if let Ok(session) = config::session() {
        state.runtime.start_receiver(format!("{session}-input"));
    }
    tracing::debug!("patched SDL3 dynamic API gamepad entries");
}

fn prepare_enumeration(state: &Sdl3TableState) -> bool {
    let mut backend = state.backend.lock();
    if let Err(error) = state.runtime.prepare_enumeration(&mut *backend) {
        tracing::debug!(message = %(error.0));
        false
    } else {
        true
    }
}

fn prepare_export_gamepads(state: &Sdl3ExportState) {
    if prepare_enumeration(&state.table)
        && !state.table.logged_enumeration.swap(true, Ordering::AcqRel)
    {
        let count = config::sdl_gamepad_count();
        tracing::debug!("enumerated {count} SDL3 gamepad(s) through frontend exports");
    }
}

unsafe fn enumerate_gamepads(
    count: *mut i32,
    next: GetGamepadsDispatchContext<'_>,
) -> *mut api::JoystickId {
    enumerate_virtual(count, next.state)
}

unsafe fn enumerate_joysticks(
    count: *mut i32,
    next: GetJoysticksDispatchContext<'_>,
) -> *mut api::JoystickId {
    enumerate_virtual(count, next.state)
}

fn enumerate_virtual(count: *mut i32, state: &Sdl3TableState) -> *mut api::JoystickId {
    prepare_enumeration(state);
    let backend = state.backend.lock();
    let result = unsafe { backend.enumerate(count) };
    if !state.logged_enumeration.swap(true, Ordering::AcqRel) {
        let count = if count.is_null() {
            0
        } else {
            unsafe { *count }
        };
        tracing::debug!("enumerated {count} SDL3 gamepad(s)");
    }
    result
}

fn flush_gamepads(state: &Sdl3TableState) {
    if !state.logged_gamepad_consumer.swap(true, Ordering::AcqRel) {
        tracing::debug!("target first consumed SDL3 gamepad state");
    }
    let mut backend = state.backend.lock();
    if state.runtime.flush(&mut *backend)
        && !state.logged_gamepad_update.swap(true, Ordering::AcqRel)
    {
        tracing::debug!("forwarded the first virtual gamepad update");
    }
}
