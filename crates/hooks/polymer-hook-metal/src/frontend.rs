use super::{
    drawable::{Drawable, Job},
    tracker::Tracker,
};
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_quartz_core::CAMetalLayer;
use polymer_presentation::{Extent2D, producer::ProducerSession, swapchain::*};
use polymer_producer_metal::swapchain::{CreateInfo, MetalVirtualBackend};
use std::{
    io,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

static ASSIGNMENTS: OnceLock<Arc<AssignmentRegistry>> = OnceLock::new();
static SERIAL: AtomicUsize = AtomicUsize::new(1);
pub(super) struct Frontend {
    owner: Option<OwnerCell<SwapchainController<MetalVirtualBackend>>>,
    _registration: SourceRegistration,
    jobs: mpsc::Sender<Job>,
}
impl Frontend {
    pub fn initialize() -> io::Result<()> {
        let configuration =
            polymer_hook::config::render_configuration().map_err(io::Error::other)?;
        let registry = AssignmentRegistry::new(configuration).map_err(io::Error::other)?;
        let _ = ASSIGNMENTS.set(registry);
        Ok(())
    }
    pub fn create(layer: &CAMetalLayer) -> io::Result<Option<Arc<Self>>> {
        let device = layer
            .device()
            .ok_or_else(|| io::Error::other("CAMetalLayer has no device"))?;
        Tracker::install(&device)?;
        let serial = SERIAL.fetch_add(1, Ordering::Relaxed) as u64;
        let source = SourceIdentity {
            device: Retained::as_ptr(&device) as usize as u64,
            object: layer as *const _ as u64,
            lifetime: serial,
        };
        let registry = ASSIGNMENTS
            .get()
            .ok_or_else(|| io::Error::other("Metal hook is not initialized"))?;
        // A layer's owning NSView supplies a window identity when available.
        let candidate = super::Hook::candidate(layer, source);
        let registration = registry.register(candidate).map_err(io::Error::other)?;
        let size = layer.drawableSize();
        let Some(provisional) = registry.reserve(
            source,
            Extent2D {
                width: size.width as u32,
                height: size.height as u32,
            },
        ) else {
            return Ok(None);
        };
        let binding = provisional.shared_binding();
        let session =
            ProducerSession::connect(&polymer_hook::config::session().map_err(io::Error::other)?)?;
        let controller = SwapchainController::create(
            CreateInfo {
                device,
                format: layer.pixelFormat(),
                image_count: layer.maximumDrawableCount() as u32,
            },
            binding,
            session,
        )
        .map_err(|failure| {
            io::Error::other(format!("Metal swapchain setup: {:?}", failure.error))
        })?;
        provisional.commit().map_err(io::Error::other)?;
        let (jobs, receiver) = mpsc::channel::<Job>();
        let frontend = Arc::new(Self {
            owner: Some(OwnerCell::new(controller)),
            _registration: registration,
            jobs,
        });
        let weak = Arc::downgrade(&frontend);
        std::thread::Builder::new()
            .name("polymer-metal-present".into())
            .spawn(move || {
                while let Ok(Job(drawable, lease, work)) = receiver.recv() {
                    let Some(frontend) = weak.upgrade() else {
                        break;
                    };
                    let result = frontend
                        .owner
                        .as_ref()
                        .unwrap()
                        .enter_until(AcquireDeadline::after(Duration::from_secs(2)))
                        .map_err(|error| format!("{error:?}"))
                        .and_then(|mut owner| {
                            owner
                                .present(lease, work)
                                .map_err(|error| format!("{error:?}"))
                        });
                    if let Err(error) = result {
                        eprintln!("Polymer Metal publication failed: {error}");
                    }
                    drawable.complete();
                }
            })?;
        Ok(Some(frontend))
    }
    pub fn next(
        self: &Arc<Self>,
        layer: &CAMetalLayer,
    ) -> io::Result<Retained<ProtocolObject<dyn objc2_quartz_core::CAMetalDrawable>>> {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if let Some(mut owner) = self.owner.as_ref().unwrap().enter() {
                match owner.acquire(None, AcquireDeadline::Nonblocking) {
                    Ok(lease) => {
                        let access = owner
                            .api_access(lease.slot())
                            .map_err(|e| io::Error::other(format!("{e:?}")))?;
                        return Ok(ProtocolObject::from_retained(Drawable::new(
                            self.clone(),
                            layer,
                            access,
                            lease,
                            SERIAL.fetch_add(1, Ordering::Relaxed),
                        )));
                    }
                    Err(ControllerError::WouldBlock) => {}
                    Err(error) => return Err(io::Error::other(format!("{error:?}"))),
                }
            }
            if Instant::now() >= deadline {
                return Err(io::Error::from(io::ErrorKind::TimedOut));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    pub fn present(&self, job: Job) -> io::Result<()> {
        self.jobs
            .send(job)
            .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))
    }
    pub fn cancel(&self, lease: AppLease) {
        if let Ok(mut owner) = self
            .owner
            .as_ref()
            .unwrap()
            .enter_until(AcquireDeadline::after(Duration::from_secs(2)))
        {
            let _ = owner.cancel(lease);
        }
    }
}
impl Drop for Frontend {
    fn drop(&mut self) {
        if let Some(controller) = self.owner.take().and_then(OwnerCell::into_inner) {
            let mut retirement = controller.retire(RetireReason::ApplicationDestroyed);
            let _ = std::thread::Builder::new()
                .name("polymer-metal-retire".into())
                .spawn(move || {
                    let deadline = Instant::now() + Duration::from_secs(30);
                    while retirement.poll() == RetirementProgress::Pending
                        && Instant::now() < deadline
                    {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    // Unproven external ownership remains quarantined by the shared retirement guard.
                });
        }
    }
}
