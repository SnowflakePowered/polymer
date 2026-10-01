use super::{BackendRetirement, RetirementProgress, SwapchainBackend};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::Arc;
use std::sync::{LazyLock, mpsc};
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::*;
use windows::Win32::System::Threading::{CreateEventW, INFINITE, SetEvent, WaitForSingleObject};

struct WorkerConnection {
    sender: mpsc::Sender<Box<dyn RetiringResource>>,
    wake: Arc<OwnedHandle>,
}
static WORKER: LazyLock<Option<WorkerConnection>> = LazyLock::new(|| {
    // A native resource owner may outlive application COM references. Keep its
    // code mapped until process exit while the DXGI retirement worker runs.
    let mut module = HMODULE::default();
    if unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN,
            windows::core::PCWSTR(DxgiMaintenance::run as *const () as *const u16),
            &mut module,
        )
    }
    .is_err()
    {
        tracing::error!("could not pin DXGI retirement code; retaining abandoned generations");
        return None;
    }
    let (sender, receiver) = mpsc::channel();
    let event = unsafe { CreateEventW(None, false, false, None) }.ok()?;
    let wake = Arc::new(unsafe { OwnedHandle::from_raw_handle(event.0) });
    let thread_wake = wake.clone();
    std::thread::Builder::new()
        .name("polymer-dxgi-maintenance".into())
        .spawn(move || DxgiMaintenance::run(receiver, thread_wake))
        .ok()?;
    Some(WorkerConnection { sender, wake })
});

pub struct DxgiMaintenance;
pub trait RetiringResource: Send {
    fn poll(&mut self) -> RetirementProgress;
}
impl<B: SwapchainBackend> RetiringResource for BackendRetirement<B>
where
    Self: Send,
{
    fn poll(&mut self) -> RetirementProgress {
        BackendRetirement::poll(self)
    }
}
impl DxgiMaintenance {
    pub fn retire(work: impl RetiringResource + 'static) {
        if let Some(worker) = &*WORKER {
            let _ = worker.sender.send(Box::new(work));
            unsafe {
                let _ = SetEvent(HANDLE(worker.wake.as_raw_handle()));
            }
        }
    }
    fn run(receiver: mpsc::Receiver<Box<dyn RetiringResource>>, wake: Arc<OwnedHandle>) {
        let mut pending = Vec::new();
        loop {
            pending.extend(receiver.try_iter());
            pending.retain_mut(|retirement| retirement.poll() == RetirementProgress::Pending);
            // Retirement waits on resources owned by other APIs and is polled
            // only while a destroyed swapchain drains.
            let timeout = if pending.is_empty() { INFINITE } else { 20 };
            if unsafe { WaitForSingleObject(HANDLE(wake.as_raw_handle()), timeout) } == WAIT_FAILED
            {
                return;
            }
        }
    }
}
