use std::{cell::Cell, time::Instant};
use windows::{
    System::DispatcherQueueController,
    Win32::{
        Foundation::WAIT_FAILED,
        Media::{TIMERR_NOERROR, timeBeginPeriod, timeEndPeriod},
        System::WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions, RO_INIT_MULTITHREADED, RO_INIT_SINGLETHREADED, RoInitialize,
            RoUninitialize,
        },
        UI::WindowsAndMessaging::{MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, QS_ALLINPUT},
    },
};

/// Initialises the Windows Runtime apartment used by the desktop visual layer.
/// The shell owns one current-thread apartment for its entire event loop.
pub struct WinRtApartment {
    _dispatcher: DispatcherQueueController,
    _apartment: RoApartment,
}

struct RoApartment;

thread_local! {
    static WORKER_WINRT_INITIALIZED: Cell<bool> = const { Cell::new(false) };
}

pub struct WorkerWinRtApartment;

impl WorkerWinRtApartment {
    pub fn initialize_current_thread() {
        // SAFETY: Tokio calls this once on each new worker thread. The matching
        // stop hook below balances every successful initialization.
        let initialized = unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.is_ok();
        WORKER_WINRT_INITIALIZED.set(initialized);
        if !initialized {
            tracing::warn!("polymer: could not initialize WinRT on a Tokio worker");
        }
    }

    pub fn uninitialize_current_thread() {
        if WORKER_WINRT_INITIALIZED.replace(false) {
            // SAFETY: Tokio invokes its stop hook on the same worker thread
            // whose successful initialization set this thread-local flag.
            unsafe { RoUninitialize() };
        }
    }
}

impl WinRtApartment {
    pub fn initialize() -> windows::core::Result<Self> {
        // SAFETY: this is the first WinRT apartment initialization on the main
        // thread and Drop balances every successful call, including S_FALSE.
        // Winit enables OLE drag/drop on this thread, which requires the STA
        // apartment. WUC objects used here are agile; initializing MTA first
        // makes winit's later OleInitialize fail with RPC_E_CHANGED_MODE.
        unsafe { RoInitialize(RO_INIT_SINGLETHREADED)? };
        let apartment = RoApartment;
        let options = DispatcherQueueOptions {
            dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_STA,
        };
        // SAFETY: creates a queue for this already initialized STA thread. The
        // retained controller keeps it alive through winit's entire event loop.
        let dispatcher = unsafe { CreateDispatcherQueueController(options) }?;
        Ok(Self {
            _dispatcher: dispatcher,
            _apartment: apartment,
        })
    }
}

impl Drop for RoApartment {
    fn drop(&mut self) {
        // SAFETY: balances the successful RoInitialize owned by this value.
        unsafe { RoUninitialize() };
    }
}

/// Holds the process timer resolution at 1ms for the lifetime of the event
/// loop so frame-pacing deadlines are not rounded to the legacy 15.6ms tick.
pub struct TimerResolution(bool);

impl TimerResolution {
    pub fn raise() -> Self {
        // SAFETY: a successful call is balanced in Drop.
        Self(unsafe { timeBeginPeriod(1) } == TIMERR_NOERROR)
    }
}

impl Drop for TimerResolution {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful `timeBeginPeriod(1)` above.
            unsafe { timeEndPeriod(1) };
        }
    }
}

pub struct NativeEventLoop;
impl NativeEventLoop {
    pub fn wait_for_events(deadline: Instant) -> Result<(), Box<dyn std::error::Error>> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(());
        }
        // Round up so a sub-millisecond remainder does not become a busy poll.
        let timeout = remaining
            .as_nanos()
            .div_ceil(1_000_000)
            .min(u128::from(u32::MAX - 1)) as u32;
        // SAFETY: waits on this thread's message queue without removing or
        // dispatching messages. INPUTAVAILABLE also observes messages queued
        // before this call, including Actix wakes sent during the async turn.
        let result =
            unsafe { MsgWaitForMultipleObjectsEx(None, timeout, QS_ALLINPUT, MWMO_INPUTAVAILABLE) };
        if result == WAIT_FAILED {
            return Err(windows::core::Error::from_win32().into());
        }
        Ok(())
    }
}
