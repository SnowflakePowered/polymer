use polymer_composition::SurfaceFrameEvent;
use polymer_platform_windows::performance_counter;
use polymer_renderer::layout::Rect;
use polymer_renderer::{
    CompositingPlan, Renderer, SurfaceContent, SurfacePlacement, SurfacePointerSender,
};
use polymer_renderer_d3d12::D3d12Compositor;
use polymer_runtime::SurfaceFrameReceiver;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
type SurfaceSnapshotResult = Result<Option<polymer_runtime::RgbaScreenshot>, String>;
type SurfaceSnapshotRequest = mpsc::SyncSender<SurfaceSnapshotResult>;

/// Owns one captured stream and its opaque WUC swapchain on a presentation
/// thread. The compositor retains the displayed producer slot until replacement
/// (or caches single-slot/keyed streams), independently of Blitz/Deno UI turns.
pub(super) struct SurfacePresentationWorker {
    stopping: Arc<AtomicBool>,
    snapshots: mpsc::Sender<SurfaceSnapshotRequest>,
    worker: Option<thread::JoinHandle<()>>,
}

impl SurfacePresentationWorker {
    #[expect(clippy::too_many_arguments)]
    pub(super) fn spawn(
        mut compositor: D3d12Compositor,
        mut frames: SurfaceFrameReceiver,
        pointer: SurfacePointerSender,
        events: mpsc::Sender<SurfaceFrameEvent>,
        connection: u64,
        surface_id: u64,
        width: u32,
        height: u32,
    ) -> Self {
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_stopping = Arc::clone(&stopping);
        let (snapshot_sender, snapshot_requests) = mpsc::channel::<SurfaceSnapshotRequest>();
        let worker = thread::Builder::new()
            .name(format!("polymer-surface-{connection}"))
            .spawn(move || {
                let rectangle = Rect::new(0, 0, width.max(1), height.max(1));
                let mut placement = SurfacePlacement::new(connection, surface_id, rectangle, 0.0);
                placement.content = SurfaceContent::fill(rectangle);
                let plan = CompositingPlan::new([0.0, 0.0, 0.0, 1.0], vec![placement]);

                while !worker_stopping.load(Ordering::Acquire) {
                    while let Ok(response) = snapshot_requests.try_recv() {
                        let snapshot = compositor
                            .snapshot_rgba()
                            .map(|snapshot| {
                                snapshot.map(|(width, height, pixels)| {
                                    polymer_runtime::RgbaScreenshot {
                                        width,
                                        height,
                                        pixels,
                                    }
                                })
                            })
                            .map_err(|error| error.to_string());
                        let _ = response.send(snapshot);
                    }
                    let timeout = if Renderer::has_pending_frame(&compositor) {
                        Duration::ZERO
                    } else {
                        Duration::from_millis(100)
                    };
                    match frames.next_frame_timeout(timeout) {
                        Ok(Some(update)) => {
                            pointer.remember_frame(connection, update);
                            Renderer::set_latest(&mut compositor, connection, update.frame);
                            let _ = events.send(SurfaceFrameEvent {
                                connection,
                                update,
                                observed_qpc: performance_counter()
                                    .unwrap_or(update.frame.timestamp_qpc),
                            });
                        }
                        Ok(None) => {}
                        Err(_) => break,
                    }

                    match compositor.wait_for_ready_pending_frame(Duration::from_millis(50)) {
                        Ok(true) if compositor.begin_frame_within(Duration::from_millis(50)) => {
                            if let Err(error) = compositor.present_with_webrtc(&plan, None) {
                                tracing::error!(
                                    "polymer: presenting surface {connection} on its worker failed: {error}"
                                );
                                break;
                            }
                        }
                        Ok(_) => {}
                        Err(error) => {
                            tracing::error!(
                                "polymer: waiting for surface {connection} failed: {error}"
                            );
                            break;
                        }
                    }
                }
            })
            .expect("failed to create a captured-surface presentation worker");
        Self {
            stopping,
            snapshots: snapshot_sender,
            worker: Some(worker),
        }
    }

    pub(super) fn snapshot(&self) -> SurfaceSnapshotResult {
        let (result, response) = mpsc::sync_channel(1);
        self.snapshots
            .send(result)
            .map_err(|_| "surface presentation worker stopped".to_owned())?;
        response
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| "surface screenshot timed out".to_owned())?
    }
}

impl Drop for SurfacePresentationWorker {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
