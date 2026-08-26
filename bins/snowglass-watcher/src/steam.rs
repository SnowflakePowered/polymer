use std::{
    ffi::{CStr, c_char, c_void},
    io, mem,
    path::PathBuf,
};

use libloading::os::windows::{
    LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, Library,
};
use windows_registry::CURRENT_USER;

use crate::lifecycle::LifecycleAdjustment;

const STEAM_KEY: &str = "Software\\Valve\\Steam";
const STEAM_PATH_VALUE: &str = "SteamPath";
const STEAM_CLIENT_DLL: &str = "steamclient64.dll";
const CLIENT_ENGINE_INTERFACE: &CStr = c"CLIENTENGINE_INTERFACE_VERSION005";
const GUIDE_BUTTON_FOCUSES_STEAM: &CStr = c"Controller_CheckGuideButton";
const CONFIG_STORE_USER_LOCAL: i32 = 3;
const CLIENT_ENGINE_GET_CONFIG_STORE_SLOT: usize = 44;
const CONFIG_STORE_GET_BOOL_SLOT: usize = 1;
const CONFIG_STORE_SET_BOOL_SLOT: usize = 9;
const ERROR_FILE_NOT_FOUND: i32 = 0x8007_0002_u32 as i32;

pub struct SteamGuideButtonFocus {
    config_store: Option<SteamConfigStore>,
    previous_value: bool,
    restore_pending: bool,
}

impl LifecycleAdjustment for SteamGuideButtonFocus {
    fn applicable() -> io::Result<bool> {
        Ok(SteamConfigStore::client_path()?.is_some())
    }

    fn disable() -> io::Result<Self> {
        let Some(config_store) = SteamConfigStore::connect()? else {
            return Ok(Self {
                config_store: None,
                previous_value: false,
                restore_pending: false,
            });
        };

        let previous_value = config_store.get_bool(GUIDE_BUTTON_FOCUSES_STEAM, true);
        if previous_value {
            config_store.set_bool(GUIDE_BUTTON_FOCUSES_STEAM, false)?;
        }

        Ok(Self {
            config_store: Some(config_store),
            previous_value,
            restore_pending: previous_value,
        })
    }

    fn restore(&mut self) -> io::Result<()> {
        if !self.restore_pending {
            return Ok(());
        }
        self.restore_pending = false;

        let Some(config_store) = &self.config_store else {
            return Ok(());
        };

        config_store.set_bool(GUIDE_BUTTON_FOCUSES_STEAM, self.previous_value)
    }
}

type CreateInterface = unsafe extern "C" fn(*const c_char, *mut i32) -> *mut c_void;
type CreateSteamPipe = unsafe extern "C" fn() -> i32;
type ConnectToGlobalUser = unsafe extern "C" fn(i32) -> i32;
type ReleaseUser = unsafe extern "C" fn(i32, i32);
type ReleaseSteamPipe = unsafe extern "C" fn(i32) -> u8;
type GetConfigStore = unsafe extern "C" fn(*mut c_void, i32, i32) -> *mut c_void;
type GetBool = unsafe extern "C" fn(*mut c_void, i32, *const c_char, u8) -> u8;
type SetBool = unsafe extern "C" fn(*mut c_void, i32, *const c_char, u8) -> u8;

struct SteamConfigStore {
    _library: Library,
    config_store: *mut c_void,
    pipe: i32,
    user: i32,
    release_user: ReleaseUser,
    release_pipe: ReleaseSteamPipe,
}

impl SteamConfigStore {
    fn connect() -> io::Result<Option<Self>> {
        let Some(client_path) = Self::client_path()? else {
            return Ok(None);
        };
        let library = unsafe {
            Library::load_with_flags(
                &client_path,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
            )
        }
        .map_err(io::Error::other)?;

        // Steam's public C exports establish a pipe to the already-running
        // client. No steam.exe process or UI command is involved.
        let create_interface: CreateInterface =
            unsafe { Self::symbol(&library, b"CreateInterface\0")? };
        let create_pipe: CreateSteamPipe =
            unsafe { Self::symbol(&library, b"Steam_CreateSteamPipe\0")? };
        let connect_user: ConnectToGlobalUser =
            unsafe { Self::symbol(&library, b"Steam_ConnectToGlobalUser\0")? };
        let release_user: ReleaseUser = unsafe { Self::symbol(&library, b"Steam_ReleaseUser\0")? };
        let release_pipe: ReleaseSteamPipe =
            unsafe { Self::symbol(&library, b"Steam_BReleaseSteamPipe\0")? };

        let mut return_code = 0;
        let engine =
            unsafe { create_interface(CLIENT_ENGINE_INTERFACE.as_ptr(), &raw mut return_code) };
        if engine.is_null() {
            return Err(io::Error::other(format!(
                "Steam client engine interface is unavailable ({return_code})"
            )));
        }

        let pipe = unsafe { create_pipe() };
        if pipe == 0 {
            return Ok(None);
        }
        let user = unsafe { connect_user(pipe) };
        if user == 0 {
            unsafe { release_pipe(pipe) };
            return Ok(None);
        }

        let get_config_store: GetConfigStore =
            unsafe { Self::virtual_method(engine, CLIENT_ENGINE_GET_CONFIG_STORE_SLOT) };
        let config_store = unsafe { get_config_store(engine, user, pipe) };
        if config_store.is_null() {
            unsafe {
                release_user(pipe, user);
                release_pipe(pipe);
            }
            return Err(io::Error::other(
                "Steam config-store interface is unavailable",
            ));
        }

        Ok(Some(Self {
            _library: library,
            config_store,
            pipe,
            user,
            release_user,
            release_pipe,
        }))
    }

    fn client_path() -> io::Result<Option<PathBuf>> {
        let key = match CURRENT_USER.open(STEAM_KEY) {
            Ok(key) => key,
            Err(error) if error.code().0 == ERROR_FILE_NOT_FOUND => return Ok(None),
            Err(error) => return Err(io::Error::other(error)),
        };
        let steam_path = match key.get_string(STEAM_PATH_VALUE) {
            Ok(path) => PathBuf::from(path),
            Err(error) if error.code().0 == ERROR_FILE_NOT_FOUND => return Ok(None),
            Err(error) => return Err(io::Error::other(error)),
        };
        let client_path = steam_path.join(STEAM_CLIENT_DLL);

        Ok(client_path.is_file().then_some(client_path))
    }

    fn get_bool(&self, key: &CStr, default_value: bool) -> bool {
        let get_bool: GetBool =
            unsafe { Self::virtual_method(self.config_store, CONFIG_STORE_GET_BOOL_SLOT) };
        unsafe {
            get_bool(
                self.config_store,
                CONFIG_STORE_USER_LOCAL,
                key.as_ptr(),
                default_value.into(),
            ) != 0
        }
    }

    fn set_bool(&self, key: &CStr, value: bool) -> io::Result<()> {
        let set_bool: SetBool =
            unsafe { Self::virtual_method(self.config_store, CONFIG_STORE_SET_BOOL_SLOT) };
        let changed = unsafe {
            set_bool(
                self.config_store,
                CONFIG_STORE_USER_LOCAL,
                key.as_ptr(),
                value.into(),
            ) != 0
        };
        if changed {
            Ok(())
        } else {
            Err(io::Error::other("Steam rejected the config-store update"))
        }
    }

    unsafe fn virtual_method<T: Copy>(interface: *mut c_void, slot: usize) -> T {
        let virtual_table = unsafe { *(interface.cast::<*const *const c_void>()) };
        let address = unsafe { *virtual_table.add(slot) };
        debug_assert_eq!(mem::size_of::<T>(), mem::size_of_val(&address));
        unsafe { mem::transmute_copy(&address) }
    }

    unsafe fn symbol<T: Copy>(library: &Library, name: &[u8]) -> io::Result<T> {
        let symbol = unsafe { library.get::<T>(name) }.map_err(io::Error::other)?;
        Ok(*symbol)
    }
}

impl Drop for SteamConfigStore {
    fn drop(&mut self) {
        unsafe {
            (self.release_user)(self.pipe, self.user);
            (self.release_pipe)(self.pipe);
        }
    }
}
