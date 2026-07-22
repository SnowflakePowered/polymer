//! SDL3 module discovery and detour ownership.

use crate::api;
use snowglass_hook::config;
use snowglass_input_sdl::Runtime;
use snowglass_platform_windows::InlineDetour;
use std::{
    ffi::c_void,
    fs::OpenOptions,
    io::Write,
    os::windows::ffi::OsStrExt,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use windows_sys::Win32::{
    Foundation::HMODULE,
    System::LibraryLoader::{GetModuleHandleW, LoadLibraryW},
};

pub(crate) struct Installation {
    dynapi_entry: InlineDetour<api::DynapiEntryFn>,
    dynapi: OnceLock<Sdl3DynapiHook>,
    backend: Mutex<api::Api>,
    runtime: Arc<Runtime>,
}

static RUNTIME: OnceLock<Arc<Runtime>> = OnceLock::new();
static INSTALLATION: OnceLock<Installation> = OnceLock::new();
static DISCOVERY_STARTED: OnceLock<()> = OnceLock::new();
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

snowglass_hook::function_table_hook! {
    #[resolve = dynapi]
    pub(crate) struct Sdl3DynapiHook {
        const MINIMUM_SLOTS: usize = DYNAPI_UPDATE_JOYSTICKS + 1;
        #[context]
        pub(crate) struct Sdl3DynapiContext {
            pub(crate) installation: &'static Installation = installation(),
        }

        #[slot = DYNAPI_GET_GAMEPADS]
        #[prefix(|next| prepare_enumeration(next.common.installation))]
        unsafe extern "C" fn get_gamepads_dispatch(
            count: *mut i32,
        ) -> *mut api::JoystickId;

        #[slot = DYNAPI_GET_JOYSTICKS]
        #[prefix(|next| prepare_enumeration(next.common.installation))]
        unsafe extern "C" fn get_joysticks_dispatch(
            count: *mut i32,
        ) -> *mut api::JoystickId;

        #[slot = DYNAPI_UPDATE_JOYSTICKS]
        #[prefix(|next| flush_gamepads(next.common.installation))]
        unsafe extern "C" fn update_joysticks_dispatch();
    }
}

fn installation() -> &'static Installation {
    INSTALLATION.get().expect("SDL3 installation exists")
}

fn dynapi() -> &'static Sdl3DynapiHook {
    installation()
        .dynapi
        .get()
        .expect("SDL3 dynamic API table was captured before dispatch")
}

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
    preload_dynamic_api();
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
    let Some(dynapi_entry_target) = (unsafe { api::symbol(module, c"SDL_DYNAPI_entry") }) else {
        return;
    };
    let Some(backend) = (unsafe { api::Api::load(module) }) else {
        log("SDL3 found, but its virtual joystick exports are unavailable");
        return;
    };
    let dynapi_entry =
        InlineDetour::new(dynapi_entry_target, dynapi_entry_hook as api::DynapiEntryFn);
    if INSTALLATION
        .set(Installation {
            dynapi_entry,
            dynapi: OnceLock::new(),
            backend: Mutex::new(backend),
            runtime: runtime(),
        })
        .is_err()
    {
        return;
    }
    let installation = INSTALLATION.get().expect("SDL3 installation was set");
    if installation.dynapi_entry.enable().is_err() {
        log("failed to enable SDL3 dynamic API detour");
        return;
    }
    log("installed SDL3 virtual gamepad hooks");
}

unsafe extern "C" fn dynapi_entry_hook(apiver: u32, table: *mut c_void, tablesize: u32) -> i32 {
    let Some(installation) = INSTALLATION.get() else {
        return -1;
    };
    let result = unsafe { (installation.dynapi_entry.trampoline())(apiver, table, tablesize) };
    if result < 0 || table.is_null() {
        return result;
    }

    if installation.dynapi.get().is_none() {
        let captured = match unsafe { Sdl3DynapiHook::capture(table, tablesize) } {
            Ok(captured) => captured,
            Err(error) => {
                log(error);
                return result;
            }
        };
        let _ = installation.dynapi.set(captured);
    }
    if let Err(error) = unsafe { dynapi().patch(table, tablesize) } {
        log(error);
        return result;
    }
    log("patched SDL3 dynamic API gamepad entries");
    result
}

fn prepare_enumeration(installation: &Installation) {
    let mut backend = installation
        .backend
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    match installation.runtime.prepare_enumeration(&mut *backend) {
        Ok(()) => {
            if ATTACHMENT_LOGGED.set(()).is_ok() {
                log("attached SDL3 virtual gamepad topology");
            }
        }
        Err(error) => log(error.0),
    }
}

fn flush_gamepads(installation: &Installation) {
    let mut backend = installation
        .backend
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    installation.runtime.flush(&mut *backend);
}

fn log(message: &str) {
    let Some(path) = config::sdl_log_file() else {
        return;
    };
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "snowglass SDL3: {message}");
    }
}
