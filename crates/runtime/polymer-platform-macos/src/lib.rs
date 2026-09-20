//! Native macOS session components. Desktop isolation runs inside the guest.
#![cfg(target_os = "macos")]

mod audio;
mod bundle;
pub mod spaces;

pub use audio::SystemAudioState;
pub use bundle::{BundleFactory, StagedBundle};

mod guest;
mod window;
pub use guest::GuestPlatform;

/// Host-side native view identity; guest window identities use WindowServer IDs.
#[derive(Clone, Copy)]
pub struct NativeWindow(pub isize);
pub fn performance_counter() -> std::io::Result<u64> {
    Ok(unsafe { mach2::mach_time::mach_absolute_time() })
}
pub fn performance_frequency() -> std::io::Result<u64> {
    let mut info = mach2::mach_time::mach_timebase_info_data_t { numer: 0, denom: 0 };
    let status = unsafe { mach2::mach_time::mach_timebase_info(&mut info) };
    if status != 0 || info.numer == 0 {
        return Err(std::io::Error::other("mach timebase unavailable"));
    }
    Ok(1_000_000_000 * u64::from(info.denom) / u64::from(info.numer))
}

pub use window::GuestEvents;
