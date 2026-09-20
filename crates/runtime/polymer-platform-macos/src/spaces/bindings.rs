//! SkyLight has no SDK headers or link stub. Keep its private C ABI here.
//! Signatures and the SpaceCreate sentinel were exercised on macOS 27; see MACOS.md.

use libloading::Library;
use objc2_foundation::{NSArray, NSNumber};
use std::{ffi::c_void, io};

pub(super) type ConnectionId = i32;
pub(super) type SpaceId = u64;
type IdArray = NSArray<NSNumber>;

pub(super) struct SkyLight {
    pub main_connection: unsafe extern "C" fn() -> ConnectionId,
    pub create: unsafe extern "C" fn(ConnectionId, *mut c_void, *const c_void) -> SpaceId,
    pub destroy: unsafe extern "C" fn(ConnectionId, SpaceId),
    pub set_type: unsafe extern "C" fn(ConnectionId, SpaceId, i32),
    pub add_windows: unsafe extern "C" fn(ConnectionId, &IdArray, &IdArray),
    pub remove_windows: unsafe extern "C" fn(ConnectionId, &IdArray, &IdArray),
    pub copy_spaces: unsafe extern "C" fn(ConnectionId, i32, &IdArray) -> *mut IdArray,
    // Keeps every resolved function alive until after the owning Space is destroyed.
    _library: Library,
}

impl SkyLight {
    pub fn load() -> io::Result<Self> {
        // SAFETY: load Apple's system framework and bind the validated C ABI.
        // NSArray<NSNumber> is toll-free bridged to the CFArray of CFNumbers CGS uses.
        unsafe {
            let library =
                Library::new("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight")
                    .map_err(io::Error::other)?;
            Ok(Self {
                main_connection: *library
                    .get(b"CGSMainConnectionID\0")
                    .map_err(io::Error::other)?,
                create: *library.get(b"CGSSpaceCreate\0").map_err(io::Error::other)?,
                destroy: *library
                    .get(b"CGSSpaceDestroy\0")
                    .map_err(io::Error::other)?,
                set_type: *library
                    .get(b"CGSSpaceSetType\0")
                    .map_err(io::Error::other)?,
                add_windows: *library
                    .get(b"CGSAddWindowsToSpaces\0")
                    .map_err(io::Error::other)?,
                remove_windows: *library
                    .get(b"CGSRemoveWindowsFromSpaces\0")
                    .map_err(io::Error::other)?,
                copy_spaces: *library
                    .get(b"CGSCopySpacesForWindows\0")
                    .map_err(io::Error::other)?,
                _library: library,
            })
        }
    }
}
