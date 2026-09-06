use super::*;
use polymer_presentation::producer::ConnectionRetry;

/// Native resources are initialized and retired only on the presenting GL context.
pub(super) struct Capture {
    native: Option<NativeCapture>,
    retry: ConnectionRetry,
    surface: SurfaceId,
    window: isize,
    extent: (u32, u32),
    window_extent: (u32, u32),
    bottom_up: bool,
}
impl Capture {
    /// The original context is gone. Never run GL destructors in another
    /// context's object namespace, even if the numeric context handle is reused.
    pub(super) fn context_destroyed(mut self) {
        let Some(native) = self.native.as_mut() else {
            return;
        };
        match native {
            NativeCapture::Ext(capture) => drop(capture.ipc.take()),
            NativeCapture::Nv(capture) => drop(capture.ipc.take()),
            NativeCapture::Pbo(capture) => drop(capture.ipc.take()),
        }
        tracing::error!(
            "polymer: retaining OpenGL capture resources after loss of their cleanup context"
        );
        std::mem::forget(self);
    }

    pub(super) unsafe fn new(
        surface: SurfaceId,
        window: isize,
        width: u32,
        height: u32,
        window_width: u32,
        window_height: u32,
        bottom_up: bool,
    ) -> Result<Self, String> {
        let mut capture = Self {
            native: None,
            retry: Default::default(),
            surface,
            window,
            extent: (width, height),
            window_extent: (window_width, window_height),
            bottom_up,
        };
        unsafe { capture.tick() };
        Ok(capture)
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
            Err(error) => tracing::debug!("polymer: OpenGL capture initialization failed: {error}"),
        }
    }

    pub(super) fn set_requirements(
        &mut self,
        width: u32,
        height: u32,
        bottom_up: bool,
        window: isize,
    ) {
        if (self.extent, self.bottom_up, self.window) != ((width, height), bottom_up, window) {
            self.native = None;
            self.retry = Default::default();
        }
        self.extent = (width, height);
        self.bottom_up = bottom_up;
        self.window = window;
    }
    pub(super) fn source_changed(&mut self) {
        self.retry.source_changed();
    }
    pub(super) fn width(&self) -> u32 {
        self.extent.0
    }
    pub(super) fn height(&self) -> u32 {
        self.extent.1
    }
    pub(super) fn surface_id(&self) -> SurfaceId {
        self.surface
    }
    pub(super) fn set_window_extent(&mut self, width: u32, height: u32) {
        self.window_extent = (width, height);
        if let Some(native) = self.native.as_mut() {
            native.set_window_extent(width, height);
        }
    }
    pub(super) unsafe fn copy_frame(
        &mut self,
        stream: StreamId,
        source: Option<&DrawCandidate>,
    ) -> Result<(), String> {
        let Some(native) = self.native.as_mut() else {
            return Ok(());
        };
        if !native.connected() {
            self.native = None;
            self.retry = ConnectionRetry::Parked;
            return Ok(());
        }
        match native {
            NativeCapture::Ext(value) => unsafe { value.copy_frame(stream, source) },
            NativeCapture::Nv(value) => unsafe { value.copy_frame(stream, source) },
            NativeCapture::Pbo(value) => unsafe { value.copy_frame(stream, source) },
        }
        .map(|_| ())
    }

    unsafe fn initialize(&self) -> Result<Option<NativeCapture>, String> {
        let name = config::session().map_err(|error| error.to_string())?;
        let Some(connection) = ProducerSession::setup_result(ProducerSession::connect(&name))
            .map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        unsafe {
            NativeCapture::new(
                self.surface,
                self.window,
                self.extent.0,
                self.extent.1,
                self.window_extent.0,
                self.window_extent.1,
                self.bottom_up,
                connection,
            )
        }
        .map(Some)
    }
}
