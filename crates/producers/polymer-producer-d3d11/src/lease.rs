use windows::{Win32::Graphics::Dxgi::IDXGIKeyedMutex, core::Interface};
/// Owns one acquired native keyed mutex until submission hands it to the viewer.
pub struct KeyedMutexLease<'a> {
    mutex: &'a IDXGIKeyedMutex,
    acquire_key: u64,
    publish_key: u64,
    armed: bool,
}
impl<'a> KeyedMutexLease<'a> {
    /// # Safety
    /// The mutex must guard the output resource submitted by this capture.
    pub unsafe fn try_acquire(
        mutex: &'a IDXGIKeyedMutex,
        acquire_key: u64,
        publish_key: u64,
    ) -> windows::core::Result<Option<Self>> {
        let result =
            unsafe { (Interface::vtable(mutex).AcquireSync)(mutex.as_raw(), acquire_key, 0) };
        if result.0 > 0 {
            return Ok(None);
        }
        result.ok()?;
        Ok(Some(Self {
            mutex,
            acquire_key,
            publish_key,
            armed: true,
        }))
    }
}
impl KeyedMutexLease<'_> {
    pub fn finish(mut self) -> Result<(), windows::core::Error> {
        unsafe { self.mutex.ReleaseSync(self.publish_key) }?;
        self.armed = false;
        Ok(())
    }
}
impl Drop for KeyedMutexLease<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = unsafe { self.mutex.ReleaseSync(self.acquire_key) };
        }
    }
}
