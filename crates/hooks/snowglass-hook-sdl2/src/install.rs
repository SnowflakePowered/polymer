//! SDL2 module discovery and detour ownership.

use crate::api;
use snowglass_hook::config;
use snowglass_input_sdl::Runtime;
use snowglass_platform_windows::InlineDetour;
use std::{
    fs::OpenOptions,
    io::Write,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use windows_sys::Win32::{Foundation::HMODULE, System::LibraryLoader::GetModuleHandleW};

struct Installation {
    enumerate: InlineDetour<api::EnumerateFn>,
    update: InlineDetour<api::UpdateFn>,
    backend: Mutex<api::Api>,
    runtime: Arc<Runtime>,
}

static RUNTIME: OnceLock<Arc<Runtime>> = OnceLock::new();
static INSTALLATION: OnceLock<Installation> = OnceLock::new();
static DISCOVERY_STARTED: OnceLock<()> = OnceLock::new();

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

fn runtime() -> Arc<Runtime> {
    RUNTIME
        .get_or_init(|| Arc::new(Runtime::new(config::sdl_gamepad_count())))
        .clone()
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn SnowglassInstallSdl2Hook() -> i32 {
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
        && let Some(module) = loaded_module(SDL2_DLL).or_else(|| loaded_module(SDL2_COMPAT_DLL))
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
    let Some(enumerate_target) = (unsafe { api::symbol(module, c"SDL_NumJoysticks") }) else {
        return;
    };
    let Some(update_target) = (unsafe { api::symbol(module, c"SDL_JoystickUpdate") }) else {
        return;
    };
    let Some(backend) = (unsafe { api::Api::load(module) }) else {
        log("SDL2 found, but its virtual joystick exports are unavailable");
        return;
    };
    let enumerate = InlineDetour::new(enumerate_target, enumerate_hook as api::EnumerateFn);
    let update = InlineDetour::new(update_target, update_hook as api::UpdateFn);
    if INSTALLATION
        .set(Installation {
            enumerate,
            update,
            backend: Mutex::new(backend),
            runtime: runtime(),
        })
        .is_err()
    {
        return;
    }
    let installation = INSTALLATION.get().expect("SDL2 installation was set");
    if installation.enumerate.enable().is_err() {
        log("failed to enable SDL2 enumeration detour");
        return;
    }
    if installation.update.enable().is_err() {
        let _ = installation.enumerate.disable();
        log("failed to enable SDL2 update detour");
        return;
    }
    log("installed SDL2 virtual gamepad hooks");
}

unsafe extern "C" fn enumerate_hook() -> i32 {
    let Some(installation) = INSTALLATION.get() else {
        return -1;
    };
    let mut backend = installation
        .backend
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Err(error) = installation.runtime.prepare_enumeration(&mut *backend) {
        log(error.0);
    }
    drop(backend);
    unsafe { (installation.enumerate.trampoline())() }
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
    unsafe { (installation.update.trampoline())() };
}

fn log(message: &str) {
    let Some(path) = config::sdl_log_file() else {
        return;
    };
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "snowglass SDL2: {message}");
    }
}
