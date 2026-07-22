//! SDL3 module discovery and detour ownership.

use crate::api;
use snowglass_hook::config;
use snowglass_input_sdl::Runtime;
use std::{
    ffi::c_void,
    fs::OpenOptions,
    io::Write,
    os::windows::ffi::OsStrExt,
    sync::{Arc, Mutex, OnceLock},
};
use windows_sys::Win32::{
    Foundation::HMODULE,
    System::LibraryLoader::{GetModuleHandleW, LoadLibraryW},
};

pub(crate) struct Sdl3TableState {
    backend: Mutex<api::Api>,
    runtime: Arc<Runtime>,
}

static DYNAPI_ENTRY_TARGET: OnceLock<api::DynapiEntryFn> = OnceLock::new();
static DYNAPI_MODULE: OnceLock<usize> = OnceLock::new();
static ATTACHMENT_LOGGED: OnceLock<()> = OnceLock::new();

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
// SDL_dynapi_procs.h and cover the SDL revision pinned by Dolphin.
const DYNAPI_GET_GAMEPADS: usize = 349;
const DYNAPI_GET_JOYSTICKS: usize = 395;
const DYNAPI_UPDATE_JOYSTICKS: usize = 954;

fn dynapi_entry_target() -> api::DynapiEntryFn {
    *DYNAPI_ENTRY_TARGET
        .get()
        .expect("SDL3 dynamic API entry target was resolved before installation")
}

fn create_table_state() -> Result<Sdl3TableState, &'static str> {
    let module = *DYNAPI_MODULE
        .get()
        .expect("SDL3 module was resolved before table installation") as HMODULE;
    let backend =
        unsafe { api::Api::load(module) }.ok_or("SDL3 virtual joystick exports are unavailable")?;
    Ok(Sdl3TableState {
        backend: Mutex::new(backend),
        runtime: Arc::new(Runtime::new(config::sdl_gamepad_count())),
    })
}

snowglass_hook::__chain_hooks! {
    #![order(reverse)]
    #![detour(snowglass_platform_windows::InlineDetour<_>)]
    #[installer]
    pub(crate) struct Sdl3EntryInstallation;
    #[registry] static DYNAPI_ENTRY_INSTALLATION;

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
    #[state: Sdl3TableState = create_table_state()?]
    #[registry] static DYNAPI_INSTALLATION;
    pub(crate) struct Sdl3DynapiHook {
        const MINIMUM_SLOTS: usize = DYNAPI_UPDATE_JOYSTICKS + 1;

        #[slot = DYNAPI_GET_GAMEPADS]
        #[prefix(|next| prepare_enumeration(
            &next.state,
        ))]
        unsafe extern "C" fn get_gamepads_dispatch(
            count: *mut i32,
        ) -> *mut api::JoystickId;

        #[slot = DYNAPI_GET_JOYSTICKS]
        #[prefix(|next| prepare_enumeration(
            &next.state,
        ))]
        unsafe extern "C" fn get_joysticks_dispatch(
            count: *mut i32,
        ) -> *mut api::JoystickId;

        #[slot = DYNAPI_UPDATE_JOYSTICKS]
        #[prefix(|next| flush_gamepads(
            &next.state,
        ))]
        unsafe extern "C" fn update_joysticks_dispatch();
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn SnowglassInstallSdl3Hook() -> i32 {
    preload_dynamic_api();
    discover_loaded_module();
    0
}

fn preload_dynamic_api() {
    let Some(path) = std::env::var_os("SDL3_DYNAMIC_API") else {
        return;
    };
    let mut wide = path.encode_wide().collect::<Vec<_>>();
    wide.push(0);
    if unsafe { LoadLibraryW(wide.as_ptr()) }.is_null() {
        log("failed to preload SDL3_DYNAMIC_API runtime");
    }
}

fn discover_loaded_module() {
    if DYNAPI_ENTRY_TARGET.get().is_none()
        && let Some(module) = loaded_module(SDL3_DLL)
    {
        unsafe { install(module) };
    }
}

fn loaded_module(name: &[u16]) -> Option<HMODULE> {
    let module = unsafe { GetModuleHandleW(name.as_ptr()) };
    (!module.is_null()).then_some(module)
}

unsafe fn install(module: HMODULE) {
    if DYNAPI_ENTRY_TARGET.get().is_some() {
        return;
    }
    let Some(dynapi_entry_target) = (unsafe { api::symbol(module, c"SDL_DYNAPI_entry") }) else {
        return;
    };
    if DYNAPI_MODULE.set(module as usize).is_err() {
        return;
    }
    if DYNAPI_ENTRY_TARGET.set(dynapi_entry_target).is_err() {
        return;
    }
    if Sdl3EntryInstallation::install().is_err() {
        log("failed to enable SDL3 dynamic API detour");
        return;
    }
    log("installed SDL3 virtual gamepad hooks");
}

unsafe fn captured_dynapi_table(table: *mut c_void, tablesize: u32, result: i32) {
    if result < 0 || table.is_null() {
        return;
    }

    let state = match unsafe { Sdl3DynapiHook::install(table, tablesize) } {
        Ok(state) => state,
        Err(error) => {
            log(error);
            return;
        }
    };
    if let Ok(session) = config::session() {
        state.runtime.start_receiver(format!("{session}-input"));
    }
    log("patched SDL3 dynamic API gamepad entries");
}

fn prepare_enumeration(state: &Sdl3TableState) {
    let mut backend = state
        .backend
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    match state.runtime.prepare_enumeration(&mut *backend) {
        Ok(()) => {
            if ATTACHMENT_LOGGED.set(()).is_ok() {
                log("attached SDL3 virtual gamepad topology");
            }
        }
        Err(error) => log(error.0),
    }
}

fn flush_gamepads(state: &Sdl3TableState) {
    let mut backend = state
        .backend
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    state.runtime.flush(&mut *backend);
}

fn log(message: &str) {
    let Some(path) = config::sdl_log_file() else {
        return;
    };
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "snowglass SDL3: {message}");
    }
}
