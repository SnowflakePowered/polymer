//! Shared SDL3 entry and function-table hooks.
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "macos")]
pub use macos::SDL_DYNAPI_entry;
#[cfg(target_os = "macos")]
use macos::backend_module;
#[cfg(target_os = "windows")]
pub use windows::PolymerInstallSdl3Hook;
#[cfg(target_os = "windows")]
use windows::backend_module;

use crate::api;
use parking_lot::Mutex;
use polymer_hook::config;
use polymer_hook_input_sdl::Runtime;
use std::{
    ffi::c_void,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
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

impl Default for Sdl3EntryState {
    fn default() -> Self {
        let module =
            backend_module().expect("SDL3 module was resolved before entry-hook installation");
        let target = unsafe { api::symbol(module, c"SDL_DYNAPI_entry") }
            .expect("SDL3 dynamic API entry target was resolved before installation");
        Self { target }
    }
}

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
    let module = backend_module().ok_or("SDL3 module is not loaded")?;
    let runtime = Arc::new(Runtime::new());
    let backend = unsafe { api::Api::load(module, Vec::new(), runtime.clone()) }
        .ok_or("SDL3 virtual joystick exports are unavailable")?;
    Ok(Sdl3TableState {
        backend: Mutex::new(backend),
        runtime,
        logged_enumeration: AtomicBool::new(false),
        logged_gamepad_consumer: AtomicBool::new(false),
        logged_gamepad_update: AtomicBool::new(false),
    })
}

polymer_hook::chain_hooks! {
    #![order(reverse)]
    #![cfg_attr(target_os = "windows", detour(polymer_platform_windows::InlineDetour<_>))]
    #![cfg_attr(target_os = "macos", detour(polymer_hook::macos::ExportDetour<_>))]
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

polymer_hook::function_table_hook! {
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

unsafe fn captured_dynapi_table(
    table: *mut c_void,
    tablesize: u32,
    result: i32,
    _context: &DynapiEntryContext<'_>,
) {
    if result < 0 || table.is_null() {
        return;
    }

    if (tablesize as usize) < (DYNAPI_WAIT_EVENT_TIMEOUT + 1) * std::mem::size_of::<usize>() {
        return;
    }
    // The guest may use the backing runtime itself. Calling its exported
    // UpdateJoysticks after patching would re-enter this hook under the lock.
    let original_update: unsafe extern "C" fn() = unsafe {
        std::mem::transmute(
            table
                .cast::<*mut c_void>()
                .add(DYNAPI_UPDATE_JOYSTICKS)
                .read(),
        )
    };
    let state = match unsafe { Sdl3DynapiHook::install(table, tablesize) } {
        Ok(state) => state,
        Err(error) => {
            tracing::debug!(message = %(error));
            return;
        }
    };
    if !std::ptr::fn_addr_eq(
        original_update,
        update_joysticks_dispatch as unsafe extern "C" fn(),
    ) {
        state.backend.lock().update_joysticks = original_update;
    }
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
