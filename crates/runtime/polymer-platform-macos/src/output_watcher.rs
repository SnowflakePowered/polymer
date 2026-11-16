use objc2_core_audio::{
    AudioObjectAddPropertyListener, AudioObjectID, AudioObjectPropertyAddress,
    AudioObjectRemovePropertyListener, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
};
use std::{ffi::c_void, io, ptr::NonNull};

type ChangeCallback = Box<dyn Fn() + Send + Sync>;

/// Reports changes of the default output device until dropped.
pub struct DefaultOutputWatcher {
    callback: Box<ChangeCallback>,
}

impl DefaultOutputWatcher {
    const ADDRESS: AudioObjectPropertyAddress = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyDefaultOutputDevice,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };

    pub fn new(on_change: impl Fn() + Send + Sync + 'static) -> io::Result<Self> {
        let watcher = Self {
            callback: Box::new(Box::new(on_change)),
        };
        let mut address = Self::ADDRESS;
        // SAFETY: the client data is the boxed callback, which outlives the
        // registration because Drop removes it before the box is released.
        let status = unsafe {
            AudioObjectAddPropertyListener(
                kAudioObjectSystemObject as AudioObjectID,
                NonNull::from(&mut address),
                Some(Self::changed),
                watcher.client_data(),
            )
        };
        if status != 0 {
            return Err(io::Error::other(format!(
                "registering the default output listener failed: {status}"
            )));
        }
        Ok(watcher)
    }

    fn client_data(&self) -> *mut c_void {
        std::ptr::from_ref::<ChangeCallback>(&self.callback)
            .cast_mut()
            .cast()
    }

    unsafe extern "C-unwind" fn changed(
        _: AudioObjectID,
        _: u32,
        _: NonNull<AudioObjectPropertyAddress>,
        data: *mut c_void,
    ) -> i32 {
        // SAFETY: `data` is the callback registered in `new`, alive until Drop.
        let callback = unsafe { &*data.cast::<ChangeCallback>() };
        callback();
        0
    }
}

impl Drop for DefaultOutputWatcher {
    fn drop(&mut self) {
        let mut address = Self::ADDRESS;
        // SAFETY: removes the listener registered in `new` with the same arguments.
        unsafe {
            AudioObjectRemovePropertyListener(
                kAudioObjectSystemObject as AudioObjectID,
                NonNull::from(&mut address),
                Some(Self::changed),
                self.client_data(),
            );
        }
    }
}
