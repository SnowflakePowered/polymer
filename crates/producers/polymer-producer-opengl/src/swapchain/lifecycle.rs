use super::*;
use polymer_presentation::producer::ConnectionRetry;

pub(super) struct Capture {
    native: Option<ExtCapture>,
    retry: ConnectionRetry,
    surface: SurfaceId,
    window: isize,
    extent: (u32, u32),
}
impl Capture {
    pub(super) fn context_destroyed(mut self) {
        let Some(native) = self.native.as_mut() else {
            return;
        };
        drop(native.ipc.take());
        tracing::error!("polymer: retaining OpenGL resources after loss of their cleanup context");
        std::mem::forget(self);
    }
    pub(super) fn new(surface: SurfaceId, window: isize, width: u32, height: u32) -> Self {
        Self {
            native: None,
            retry: Default::default(),
            surface,
            window,
            extent: (width, height),
        }
    }
    pub(super) unsafe fn tick(&mut self) {
        if self
            .native
            .as_ref()
            .is_some_and(|native| !native.connected())
        {
            self.native = None;
            self.retry = ConnectionRetry::Parked;
        }
        if self.native.is_some() || !self.retry.begin() {
            return;
        }
        match unsafe { self.initialize() } {
            Ok(Some(native)) => self.native = Some(native),
            Ok(None) => self.retry = ConnectionRetry::Parked,
            Err(error) => {
                tracing::error!("polymer: OpenGL EXT capture initialization failed: {error}")
            }
        }
    }
    pub(super) unsafe fn copy_frame(&mut self) {
        if let Some(native) = self.native.as_mut() {
            unsafe { native.copy_frame() };
        }
    }
    unsafe fn initialize(&self) -> Result<Option<ExtCapture>, String> {
        let name = config::session().map_err(|error| error.to_string())?;
        let Some(connection) = ProducerSession::setup_result(ProducerSession::connect(&name))
            .map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        unsafe {
            ExtCapture::new(
                self.surface,
                self.window,
                self.extent.0,
                self.extent.1,
                connection,
            )
        }
        .map(Some)
    }
}
