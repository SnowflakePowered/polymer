use super::*;
use polymer_presentation::producer::CaptureSession;
use std::time::Instant;

/// Native resources are initialized and retired only on the presenting GL context.
pub(super) struct Capture {
    session: CaptureSession<NativeCapture, ((u32, u32), bool, isize)>,
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
        let Some(native) = self.session.as_mut() else {
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
            session: CaptureSession::default(),
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
        self.session
            .configure((self.extent, self.bottom_up, self.window));
        let setup = CaptureSetup {
            surface: self.surface,
            window: self.window,
            extent: self.extent,
            window_extent: self.window_extent,
            bottom_up: self.bottom_up,
        };
        if let Err(error) = unsafe { self.session.initialize(Instant::now(), &setup) } {
            tracing::debug!("polymer: OpenGL capture initialization failed: {error}");
        }
    }

    pub(super) fn set_requirements(
        &mut self,
        width: u32,
        height: u32,
        bottom_up: bool,
        window: isize,
    ) {
        self.extent = (width, height);
        self.bottom_up = bottom_up;
        self.window = window;
        self.session.configure((self.extent, bottom_up, window));
    }
    pub(super) fn source_changed(&mut self) {
        self.session.source_changed();
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
        if let Some(native) = self.session.as_mut() {
            native.set_window_extent(width, height);
        }
    }
    pub(super) unsafe fn copy_frame(
        &mut self,
        stream: StreamId,
        source: Option<&DrawCandidate>,
    ) -> Result<(), String> {
        unsafe { self.session.capture(CopyFrame { stream, source }) }.map(|_| ())
    }
}

impl polymer_presentation::producer::CaptureBackend for NativeCapture {
    fn connected(&self) -> bool {
        NativeCapture::connected(self)
    }
}

struct CaptureSetup {
    surface: SurfaceId,
    window: isize,
    extent: (u32, u32),
    window_extent: (u32, u32),
    bottom_up: bool,
}
impl polymer_presentation::producer::CaptureInitializer for CaptureSetup {
    type Backend = NativeCapture;
    type Error = String;
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

struct CopyFrame<'a> {
    stream: StreamId,
    source: Option<&'a DrawCandidate>,
}
impl polymer_presentation::producer::CaptureOperation for CopyFrame<'_> {
    type Backend = NativeCapture;
    type Output = ();
    type Error = String;
    unsafe fn submit(self, backend: &mut NativeCapture) -> Result<Option<()>, String> {
        match backend {
            NativeCapture::Ext(value) => unsafe { value.copy_frame(self.stream, self.source) },
            NativeCapture::Nv(value) => unsafe { value.copy_frame(self.stream, self.source) },
            NativeCapture::Pbo(value) => unsafe { value.copy_frame(self.stream, self.source) },
        }
    }
}
