use parking_lot::Mutex;
use std::collections::HashMap;
use windows_sys::Win32::Foundation::HANDLE;

#[derive(Default)]
pub(crate) struct SyntheticHandles(Mutex<HashMap<usize, u16>>);

impl SyntheticHandles {
    pub(crate) fn insert(&self, handle: HANDLE, product_id: u16) {
        self.0.lock().insert(handle as usize, product_id);
    }

    pub(crate) fn product_id(&self, handle: HANDLE) -> Option<u16> {
        self.0.lock().get(&(handle as usize)).copied()
    }

    pub(crate) fn remove(&self, handle: HANDLE) {
        self.0.lock().remove(&(handle as usize));
    }
}
