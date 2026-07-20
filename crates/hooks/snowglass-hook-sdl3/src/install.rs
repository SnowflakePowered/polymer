//! SDL3 module discovery and detour ownership.

use crate::api;
use retour::GenericDetour;
use snowglass_hook::config;
use snowglass_input_sdl::Runtime;
use std::{
    fs::OpenOptions,
    io::Write,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use windows_sys::Win32::{Foundation::HMODULE, System::LibraryLoader::GetModuleHandleW};

struct Installation {
    enumerate_joysticks: GenericDetour<api::EnumerateFn>,
    enumerate_gamepads: GenericDetour<api::GamepadEnumerateFn>,
    update: GenericDetour<api::UpdateFn>,
    backend: Mutex<api::Api>,
    runtime: Arc<Runtime>,
}

static RUNTIME: OnceLock<Arc<Runtime>> = OnceLock::new();
static INSTALLATION: OnceLock<Installation> = OnceLock::new();
static DISCOVERY_STARTED: OnceLock<()> = OnceLock::new();

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

fn runtime() -> Arc<Runtime> {
    RUNTIME
        .get_or_init(|| Arc::new(Runtime::new(config::sdl_gamepad_count())))
        .clone()
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn SnowglassInstallSdl3Hook() -> i32 {
    let runtime = runtime();
    if let Ok(session) = config::session() {
        runtime.start_receiver(format!("{session}-input"));
    }
    discover_loaded_module();
    if DISCOVERY_STARTED.set(()).is_ok() {
        std::thread::spawn(|| {
            loop {
                discover_loaded_module();
                std::thread::sleep(Duration::from_millis(50));
            }
        });
    }
    0
}

fn discover_loaded_module() {
    if INSTALLATION.get().is_none()
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
    if INSTALLATION.get().is_some() {
        return;
    }
    let Some(enumerate_joysticks_target) = (unsafe { api::symbol(module, c"SDL_GetJoysticks") })
    else {
        return;
    };
    let Some(enumerate_gamepads_target) = (unsafe { api::symbol(module, c"SDL_GetGamepads") })
    else {
        return;
    };
    let Some(update_target) = (unsafe { api::symbol(module, c"SDL_UpdateJoysticks") }) else {
        return;
    };
    let Some(backend) = (unsafe { api::Api::load(module) }) else {
        log("SDL3 found, but its virtual joystick exports are unavailable");
        return;
    };
    let Ok(enumerate_joysticks) = (unsafe {
        GenericDetour::new(
            enumerate_joysticks_target,
            enumerate_joysticks_hook as api::EnumerateFn,
        )
    }) else {
        log("failed to create SDL3 joystick enumeration detour");
        return;
    };
    let Ok(enumerate_gamepads) = (unsafe {
        GenericDetour::new(
            enumerate_gamepads_target,
            enumerate_gamepads_hook as api::GamepadEnumerateFn,
        )
    }) else {
        log("failed to create SDL3 gamepad enumeration detour");
        return;
    };
    let Ok(update) = (unsafe { GenericDetour::new(update_target, update_hook as api::UpdateFn) })
    else {
        log("failed to create SDL3 update detour");
        return;
    };
    if INSTALLATION
        .set(Installation {
            enumerate_joysticks,
            enumerate_gamepads,
            update,
            backend: Mutex::new(backend),
            runtime: runtime(),
        })
        .is_err()
    {
        return;
    }
    let installation = INSTALLATION.get().expect("SDL3 installation was set");
    if unsafe { installation.enumerate_joysticks.enable() }.is_err() {
        log("failed to enable SDL3 joystick enumeration detour");
        return;
    }
    if unsafe { installation.enumerate_gamepads.enable() }.is_err() {
        let _ = unsafe { installation.enumerate_joysticks.disable() };
        log("failed to enable SDL3 gamepad enumeration detour");
        return;
    }
    if unsafe { installation.update.enable() }.is_err() {
        let _ = unsafe { installation.enumerate_gamepads.disable() };
        let _ = unsafe { installation.enumerate_joysticks.disable() };
        log("failed to enable SDL3 update detour");
        return;
    }
    log("installed SDL3 virtual gamepad hooks");
}

unsafe extern "C" fn enumerate_joysticks_hook(count: *mut i32) -> *mut api::JoystickId {
    let Some(installation) = INSTALLATION.get() else {
        return std::ptr::null_mut();
    };
    prepare_enumeration(installation);
    unsafe { installation.enumerate_joysticks.call(count) }
}

unsafe extern "C" fn enumerate_gamepads_hook(count: *mut i32) -> *mut api::JoystickId {
    let Some(installation) = INSTALLATION.get() else {
        return std::ptr::null_mut();
    };
    prepare_enumeration(installation);
    unsafe { installation.enumerate_gamepads.call(count) }
}

fn prepare_enumeration(installation: &Installation) {
    let mut backend = installation
        .backend
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Err(error) = installation.runtime.prepare_enumeration(&mut *backend) {
        log(error.0);
    }
}

unsafe extern "C" fn update_hook() {
    let Some(installation) = INSTALLATION.get() else {
        return;
    };
    let mut backend = installation
        .backend
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    installation.runtime.flush(&mut *backend);
    drop(backend);
    unsafe { installation.update.call() };
}

fn log(message: &str) {
    let Some(path) = config::sdl_log_file() else {
        return;
    };
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "snowglass SDL3: {message}");
    }
}
