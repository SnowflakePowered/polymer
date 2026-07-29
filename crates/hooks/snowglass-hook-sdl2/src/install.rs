//! SDL2 module discovery and detour ownership.

use crate::api;
use parking_lot::Mutex;
use snowglass_hook::config;
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

pub(crate) struct Sdl2TableState {
    backend: Mutex<api::Api>,
    runtime: Arc<Runtime>,
    logged_enumeration: AtomicBool,
    logged_gamepad_poll: AtomicBool,
    logged_gamepad_update: AtomicBool,
}

pub(crate) struct Sdl2EntryState {
    module: snowglass_hook::__PointerKey,
    target: api::DynapiEntryFn,
}

impl Default for Sdl2EntryState {
    fn default() -> Self {
        let module =
            loaded_sdl2_module().expect("SDL2 module was resolved before entry-hook installation");
        let target = unsafe { api::symbol(module, c"SDL_DYNAPI_entry") }
            .expect("SDL2 dynamic API entry target was resolved before installation");
        Self {
            module: snowglass_hook::__PointerKey::new(module.cast())
                .expect("SDL2 module was resolved as non-null"),
            target,
        }
    }
}

const SDL2_DLL: &[u16] = &[
    b'S' as u16,
    b'D' as u16,
    b'L' as u16,
    b'2' as u16,
    b'.' as u16,
    b'd' as u16,
    b'l' as u16,
    b'l' as u16,
    0,
];
const SDL2_COMPAT_DLL: &[u16] = &[
    b'S' as u16,
    b'D' as u16,
    b'L' as u16,
    b'2' as u16,
    b'-' as u16,
    b'2' as u16,
    b'.' as u16,
    b'0' as u16,
    b'.' as u16,
    b'd' as u16,
    b'l' as u16,
    b'l' as u16,
    0,
];

// SDL2's dynamic API table is append-only, but its prefix is platform
// dependent. These are the Windows-preprocessed slots from
// SDL_dynapi_procs.h in the runtime staged with this hook.
const DYNAPI_NUM_JOYSTICKS: usize = 158;
const DYNAPI_GAME_CONTROLLER_GET_AXIS: usize = 112;
const DYNAPI_GAME_CONTROLLER_GET_BUTTON: usize = 116;
const DYNAPI_JOYSTICK_UPDATE: usize = 172;

fn dynapi_entry_target() -> api::DynapiEntryFn {
    Sdl2EntryInstallation::with_state(|state| state.target)
        .expect("SDL2 entry-hook state was installed before target resolution")
}

fn create_table_state() -> Result<Sdl2TableState, &'static str> {
    let module = Sdl2EntryInstallation::with_state(|state| state.module)
        .ok_or("SDL2 entry-hook state is not installed")?
        .as_ptr();
    let backend =
        unsafe { api::Api::load(module) }.ok_or("SDL2 virtual joystick exports are unavailable")?;
    Ok(Sdl2TableState {
        backend: Mutex::new(backend),
        runtime: Arc::new(Runtime::new(config::sdl_gamepad_count())),
        logged_enumeration: AtomicBool::new(false),
        logged_gamepad_poll: AtomicBool::new(false),
        logged_gamepad_update: AtomicBool::new(false),
    })
}

snowglass_hook::chain_hooks! {
    #![order(reverse)]
    #![detour(snowglass_platform_windows::InlineDetour<_>)]
    #[installer(state = Sdl2EntryState)]
    pub(crate) struct Sdl2EntryInstallation;

    #[trampoline]
    #[terminal(installed(target = dynapi_entry_target()), fallback = -1)]
    #[postfix(captured_dynapi_table(table, tablesize, result))]
    unsafe extern "C" fn dynapi_entry(
        apiver: u32,
        table: *mut c_void,
        tablesize: u32,
    ) -> i32;
}

snowglass_hook::function_table_hook! {
    #[state: Sdl2TableState = create_table_state()?]
    pub(crate) struct Sdl2DynapiHook {
        const MINIMUM_SLOTS: usize = DYNAPI_JOYSTICK_UPDATE + 1;

        #[slot = DYNAPI_NUM_JOYSTICKS]
        #[infix(enumerate_joysticks)]
        unsafe extern "C" fn num_joysticks_dispatch() -> i32;

        #[slot = DYNAPI_GAME_CONTROLLER_GET_AXIS]
        #[prefix(|next| flush_gamepads(
            next.state,
        ))]
        unsafe extern "C" fn game_controller_get_axis_dispatch(
            controller: *mut c_void,
            axis: i32,
        ) -> i16;

        #[slot = DYNAPI_GAME_CONTROLLER_GET_BUTTON]
        #[prefix(|next| flush_gamepads(
            next.state,
        ))]
        unsafe extern "C" fn game_controller_get_button_dispatch(
            controller: *mut c_void,
            button: i32,
        ) -> u8;

        #[slot = DYNAPI_JOYSTICK_UPDATE]
        #[prefix(|next| flush_gamepads(
            next.state,
        ))]
        unsafe extern "C" fn joystick_update_dispatch();
    }
}

#[unsafe(no_mangle)]
/// Installs the SDL2 virtual-gamepad hooks into the current process.
///
/// # Safety
///
/// The caller must invoke this only after the DLL has been loaded into a
/// Windows process whose module list and dynamic API table are valid.
pub unsafe extern "system" fn SnowglassInstallSdl2Hook() -> i32 {
    snowglass_hook::telemetry::init_file_tracing(config::sdl_log_file());
    preload_dynamic_api();
    discover_loaded_module();
    0
}

fn preload_dynamic_api() {
    let Some(path) = std::env::var_os("SDL_DYNAMIC_API") else {
        return;
    };
    let mut wide = path.encode_wide().collect::<Vec<_>>();
    wide.push(0);
    if unsafe { LoadLibraryW(wide.as_ptr()) }.is_null() {
        tracing::debug!("failed to preload SDL_DYNAMIC_API runtime");
    }
}

fn discover_loaded_module() {
    if let Some(module) = loaded_sdl2_module()
        && (unsafe { api::symbol::<api::DynapiEntryFn>(module, c"SDL_DYNAPI_entry") }).is_some()
    {
        install();
    }
}

fn loaded_sdl2_module() -> Option<HMODULE> {
    loaded_module(SDL2_DLL).or_else(|| loaded_module(SDL2_COMPAT_DLL))
}

fn loaded_module(name: &[u16]) -> Option<HMODULE> {
    let module = unsafe { GetModuleHandleW(name.as_ptr()) };
    (!module.is_null()).then_some(module)
}

fn install() {
    if Sdl2EntryInstallation::install().is_err() {
        tracing::debug!("failed to enable SDL2 dynamic API detour");
        return;
    }
    tracing::debug!("installed SDL2 virtual gamepad hooks");
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
    let state = match unsafe { Sdl2DynapiHook::install(table, tablesize) } {
        Ok(state) => state,
        Err(error) => {
            tracing::debug!(message = %(error));
            return;
        }
    };
    if let Ok(session) = config::session() {
        state.runtime.start_receiver(format!("{session}-input"));
    }
    tracing::debug!("patched SDL2 dynamic API gamepad entries");
}

unsafe fn enumerate_joysticks(mut next: NumJoysticksDispatchContext<'_>) -> i32 {
    let mut backend = next.state.backend.lock();
    if let Err(error) = next.state.runtime.prepare_enumeration(&mut *backend) {
        tracing::debug!(message = %(error.0));
    }
    drop(backend);
    // Virtual joysticks are appended to SDL's existing device list. Returning
    // only the virtual-device count makes callers inspect the wrong indices
    // whenever the process already has a physical joystick.
    let log_enumeration = !next.state.logged_enumeration.swap(true, Ordering::AcqRel);
    let count = snowglass_hook::next!(next);
    if log_enumeration {
        tracing::debug!("enumerated {count} SDL2 joystick(s)");
    }
    count
}

fn flush_gamepads(state: &Sdl2TableState) {
    if !state.logged_gamepad_poll.swap(true, Ordering::AcqRel) {
        tracing::debug!("target first polled the virtual gamepad");
    }
    let mut backend = state.backend.lock();
    if state.runtime.flush(&mut *backend)
        && !state.logged_gamepad_update.swap(true, Ordering::AcqRel)
    {
        tracing::debug!("forwarded the first virtual gamepad update");
    }
}
