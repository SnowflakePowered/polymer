use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT},
    Graphics::DirectComposition::DCompositionWaitForCompositorClock,
    System::Threading::{CreateEventW, SetEvent},
};

use polymer_composition::WindowCommand;

const TICK_TIMEOUT: Duration = Duration::from_millis(33);
/// Wakes the event loop once per DirectComposition frame while captured
/// surfaces are visible.
pub(crate) struct CompositorClock {
    worker: Option<std::thread::JoinHandle<()>>,
    stop_event: HANDLE,
    active: Arc<AtomicBool>,
    healthy: Arc<AtomicBool>,
    tick_pending: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
}

impl CompositorClock {
    pub(crate) fn spawn(
        wake: Arc<dyn Fn(WindowCommand) -> bool + Send + Sync>,
    ) -> std::io::Result<Self> {
        let stop_event =
            unsafe { CreateEventW(None, true, false, None) }.map_err(std::io::Error::other)?;
        let active = Arc::new(AtomicBool::new(false));
        let healthy = Arc::new(AtomicBool::new(true));
        let tick_pending = Arc::new(AtomicBool::new(false));
        let stopping = Arc::new(AtomicBool::new(false));
        let thread_active = Arc::clone(&active);
        let thread_healthy = Arc::clone(&healthy);
        let thread_tick_pending = Arc::clone(&tick_pending);
        let thread_stopping = Arc::clone(&stopping);
        let thread_stop_event = stop_event.0 as usize;
        let handle = match std::thread::Builder::new()
            .name("polymer-compositor-clock".to_owned())
            .spawn(move || {
                let thread_stop_event = HANDLE(thread_stop_event as *mut _);
                loop {
                    while !thread_active.load(Ordering::Acquire) {
                        if thread_stopping.load(Ordering::Acquire) {
                            return;
                        }
                        std::thread::park();
                    }
                    if thread_stopping.load(Ordering::Acquire) {
                        return;
                    }
                    let status = unsafe {
                        DCompositionWaitForCompositorClock(
                            Some(&[thread_stop_event]),
                            TICK_TIMEOUT.as_millis() as u32,
                        )
                    };
                    if status == WAIT_FAILED.0 {
                        thread_healthy.store(false, Ordering::Release);
                        return;
                    }
                    if thread_stopping.load(Ordering::Acquire) {
                        return;
                    }
                    if status != WAIT_OBJECT_0.0 + 1 && status != WAIT_TIMEOUT.0 {
                        std::thread::sleep(TICK_TIMEOUT);
                        continue;
                    }
                    if !thread_tick_pending.swap(true, Ordering::AcqRel)
                        && !wake(WindowCommand::CompositorTick)
                    {
                        return;
                    }
                }
            }) {
            Ok(handle) => handle,
            Err(error) => {
                let _ = unsafe { CloseHandle(stop_event) };
                return Err(error);
            }
        };
        Ok(Self {
            worker: Some(handle),
            stop_event,
            active,
            healthy,
            tick_pending,
            stopping,
        })
    }

    pub(crate) fn set_active(&self, active: bool) {
        if self.active.swap(active, Ordering::AcqRel) != active
            && active
            && let Some(worker) = &self.worker
        {
            worker.thread().unpark();
        }
    }

    pub(crate) fn is_pacing(&self) -> bool {
        self.healthy.load(Ordering::Acquire) && self.active.load(Ordering::Acquire)
    }

    pub(crate) fn clear_tick(&self) {
        self.tick_pending.store(false, Ordering::Release);
    }
}

impl Drop for CompositorClock {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        self.active.store(true, Ordering::Release);
        let _ = unsafe { SetEvent(self.stop_event) };
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
        let _ = unsafe { CloseHandle(self.stop_event) };
    }
}
