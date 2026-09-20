use super::*;
pub(super) fn backend_module() -> Option<api::Module> {
    // The handle is deliberately retained for every installed table's lifetime.
    static MODULE: std::sync::OnceLock<Option<libloading::Library>> = std::sync::OnceLock::new();
    MODULE
        .get_or_init(|| {
            let path = std::env::var_os("POLYMER_SDL3_RUNTIME")?;
            let library = unsafe { libloading::Library::new(path).ok()? };
            let entry = unsafe {
                library
                    .get::<api::DynapiEntryFn>(b"SDL_DYNAPI_entry\0")
                    .ok()?
            };
            if std::ptr::fn_addr_eq(*entry, SDL_DYNAPI_entry as api::DynapiEntryFn) {
                return None;
            }
            Some(library)
        })
        .as_ref()
}

#[unsafe(no_mangle)]
/// SDL loads this export through SDL3_DYNAMIC_API before its first API call.
///
/// # Safety
/// The table must be writable for `tablesize` bytes, as required by SDL's ABI.
pub unsafe extern "C" fn SDL_DYNAPI_entry(apiver: u32, table: *mut c_void, tablesize: u32) -> i32 {
    if table.is_null()
        || (tablesize as usize) < (DYNAPI_WAIT_EVENT_TIMEOUT + 1) * std::mem::size_of::<usize>()
    {
        return -1;
    }
    if backend_module().is_none() || Sdl3EntryInstallation::install().is_err() {
        eprintln!("Polymer SDL3: cannot install the dynamic API hook");
        return -1;
    }
    unsafe { dynapi_entry(apiver, table, tablesize) }
}
