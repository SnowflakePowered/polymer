//! Private, unmanaged WindowServer Spaces, owned by the injected guest.

mod bindings;

use bindings::{ConnectionId, SkyLight, SpaceId};
use objc2::{MainThreadMarker, rc::Retained};
use objc2_foundation::{NSArray, NSNumber};
use std::{ffi::c_void, io};

/// A Space deliberately absent from every managed display's Space list.
/// Must be created and used on the guest's AppKit thread, never in the host.
/// This isolates presentation and focus; it is not an OS input boundary.
pub struct PrivateSpace {
    api: SkyLight,
    connection: ConnectionId,
    id: SpaceId,
    _main_thread: MainThreadMarker,
}

impl PrivateSpace {
    pub fn new(main_thread: MainThreadMarker) -> io::Result<Self> {
        let api = SkyLight::load()?;
        // SAFETY: the loaded functions use the validated SkyLight ABI. The sentinel
        // is an opaque flag, not a dereferenceable pointer; NULL silently fails.
        let connection = unsafe { (api.main_connection)() };
        let id = unsafe { (api.create)(connection, 1usize as *mut c_void, std::ptr::null()) };
        if id == 0 {
            return Err(io::Error::other("CGSSpaceCreate returned no Space"));
        }
        unsafe { (api.set_type)(connection, id, 0) };
        Ok(Self {
            api,
            connection,
            id,
            _main_thread: main_thread,
        })
    }

    /// Move an already ordered-in window, preserving Accessibility enumeration.
    /// The caller must own the window in this process.
    pub fn capture_window(&self, window: u32) -> io::Result<()> {
        let windows = NSArray::from_slice(&[&*NSNumber::new_u32(window)]);
        let mine = NSArray::from_slice(&[&*NSNumber::new_u64(self.id)]);
        let previous = self.spaces_for_window(&windows)?;
        if previous.len() == 1 && previous[0] == self.id {
            return Ok(());
        }
        // Remove existing memberships, then add the private Space. CGS may
        // re-home an orphaned window, so the add must never be omitted.
        let old = previous
            .into_iter()
            .filter(|&id| id != self.id)
            .map(NSNumber::new_u64)
            .collect::<Vec<_>>();
        if !old.is_empty() {
            let old = NSArray::from_retained_slice(&old);
            unsafe { (self.api.remove_windows)(self.connection, &windows, &old) };
        }
        unsafe { (self.api.add_windows)(self.connection, &windows, &mine) };
        let current = self.spaces_for_window(&windows)?;
        // Unmanaged Spaces may be omitted from the query result. Reject any
        // remaining membership elsewhere, rather than requiring our ID back.
        if current.iter().any(|&id| id != self.id) {
            return Err(io::Error::other(format!(
                "window {window} did not enter Space {} (memberships: {current:?})",
                self.id
            )));
        }
        Ok(())
    }

    fn spaces_for_window(&self, windows: &NSArray<NSNumber>) -> io::Result<Vec<SpaceId>> {
        // SAFETY: Copy returns a +1 CFArray of CFNumbers, toll-free bridged to
        // NSArray<NSNumber>. Retained releases exactly that ownership.
        let spaces =
            unsafe { Retained::from_raw((self.api.copy_spaces)(self.connection, 0x7, windows)) }
                .ok_or_else(|| io::Error::other("CGSCopySpacesForWindows returned NULL"))?;
        Ok(spaces.iter().map(|number| number.as_u64()).collect())
    }
}

impl Drop for PrivateSpace {
    fn drop(&mut self) {
        // SAFETY: this connection owns the Space; the framework is still loaded.
        unsafe { (self.api.destroy)(self.connection, self.id) };
    }
}
