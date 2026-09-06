use super::*;
impl CaptureBackend for () {
    fn connected(&self) -> bool {
        true
    }
}
#[test]
fn failed_initialization_backs_off_and_rejected_stream_requires_invalidation() {
    let now = Instant::now();
    let mut stream = CaptureSession::<()>::default();
    assert!(stream.begin(now));
    assert!(!stream.begin(now + Duration::from_millis(999)));
    assert!(stream.begin(now + Duration::from_secs(1)));
    stream.park();
    assert!(!stream.begin(now + Duration::from_secs(60)));
    stream.invalidate();
    assert!(stream.begin(now));
}
struct Backend {
    connected: std::rc::Rc<std::cell::Cell<bool>>,
    retired: std::rc::Rc<std::cell::Cell<usize>>,
}
impl CaptureBackend for Backend {
    fn connected(&self) -> bool {
        self.connected.get()
    }
}
impl Drop for Backend {
    fn drop(&mut self) {
        self.retired.set(self.retired.get() + 1);
    }
}
struct Setup {
    attempts: std::cell::Cell<usize>,
    outcome: std::cell::Cell<u8>,
    connected: std::rc::Rc<std::cell::Cell<bool>>,
    retired: std::rc::Rc<std::cell::Cell<usize>>,
}
struct Operation<'a> {
    calls: &'a std::cell::Cell<usize>,
    outcome: u8,
    thread: std::thread::ThreadId,
}
impl CaptureOperation for Operation<'_> {
    type Backend = Backend;
    type Output = u64;
    type Error = ();
    unsafe fn submit(self, backend: &mut Backend) -> Result<Option<u64>, ()> {
        assert_eq!(std::thread::current().id(), self.thread);
        self.calls.set(self.calls.get() + 1);
        match self.outcome {
            0 => Ok(None), // Busy output slot.
            1 => Err(()),  // Native submission failed.
            2 => Ok(Some(42)),
            _ => {
                backend.connected.set(false); // Publication failed.
                Ok(Some(43))
            }
        }
    }
}
#[test]
fn capture_dispatch_skips_inactive_streams_and_parks_publication_failure_inline() {
    let calls = std::cell::Cell::new(0);
    let operation = |outcome| Operation {
        calls: &calls,
        outcome,
        thread: std::thread::current().id(),
    };
    let mut session = CaptureSession::<Backend>::default();
    assert_eq!(unsafe { session.capture(operation(2)) }, Ok(None));
    assert_eq!(calls.get(), 0);
    let retired = std::rc::Rc::new(std::cell::Cell::new(0));
    session.state = LifecycleState::Active(Box::new(Backend {
        connected: std::rc::Rc::new(std::cell::Cell::new(true)),
        retired: retired.clone(),
    }));
    assert_eq!(unsafe { session.capture(operation(0)) }, Ok(None));
    assert_eq!(unsafe { session.capture(operation(1)) }, Err(()));
    assert_eq!(unsafe { session.capture(operation(2)) }, Ok(Some(42)));
    assert_eq!(retired.get(), 0);
    assert_eq!(unsafe { session.capture(operation(3)) }, Ok(Some(43)));
    assert!(session.is_parked());
    assert_eq!(retired.get(), 1);
    assert_eq!(unsafe { session.capture(operation(2)) }, Ok(None));
    assert_eq!(calls.get(), 4);
    assert_eq!(
        session.counters(),
        CaptureCounters {
            completed_operations: 2,
            skipped_operations: 3,
            failures: 1,
        }
    );
}
impl CaptureInitializer for Setup {
    type Backend = Backend;
    type Error = ();
    unsafe fn initialize(&self) -> Result<Option<Backend>, ()> {
        self.attempts.set(self.attempts.get() + 1);
        match self.outcome.get() {
            0 => Err(()),
            1 => Ok(None),
            _ => Ok(Some(Backend {
                connected: self.connected.clone(),
                retired: self.retired.clone(),
            })),
        }
    }
}
#[test]
fn configuration_changes_reset_backoff_and_rejection_without_rebuilding_page_flips() {
    let now = Instant::now();
    let setup = Setup {
        attempts: Default::default(),
        outcome: Default::default(),
        connected: std::rc::Rc::new(std::cell::Cell::new(true)),
        retired: Default::default(),
    };
    let mut session = CaptureSession::<Backend, (u32, u32)>::default();
    assert!(session.configure((320, 240)));
    assert!(unsafe { session.initialize(now, &setup) }.is_err());
    assert!(!session.configure((320, 240)));
    unsafe { session.initialize(now + Duration::from_millis(500), &setup) }.unwrap();
    assert_eq!(setup.attempts.get(), 1);
    setup.outcome.set(2);
    assert!(session.configure((640, 480)));
    unsafe { session.initialize(now + Duration::from_millis(500), &setup) }.unwrap();
    assert_eq!(setup.attempts.get(), 2);
    session.source_changed();
    assert!(!session.configure((640, 480)));
    assert_eq!(setup.retired.get(), 0);
    session.park();
    assert!(!session.configure((640, 480)));
    unsafe { session.initialize(now + Duration::from_secs(60), &setup) }.unwrap();
    assert_eq!(setup.attempts.get(), 2);
    assert!(session.configure((800, 600)));
    unsafe { session.initialize(now + Duration::from_secs(60), &setup) }.unwrap();
    assert_eq!(setup.attempts.get(), 3);
    assert_eq!(setup.retired.get(), 1);
    session.park();
    session.source_changed();
    unsafe { session.initialize(now + Duration::from_secs(61), &setup) }.unwrap();
    assert_eq!(setup.attempts.get(), 4);
}
#[test]
fn retry_rejection_and_disconnect_do_not_allocate_or_disturb_siblings() {
    let now = Instant::now();
    let setup = Setup {
        attempts: Default::default(),
        outcome: Default::default(),
        connected: std::rc::Rc::new(std::cell::Cell::new(true)),
        retired: Default::default(),
    };
    let mut session = CaptureSession::<Backend>::default();
    assert!(unsafe { session.initialize(now, &setup) }.is_err());
    setup.outcome.set(1);
    unsafe { session.initialize(now + Duration::from_millis(999), &setup) }.unwrap();
    assert_eq!(setup.attempts.get(), 1);
    unsafe { session.initialize(now + Duration::from_secs(1), &setup) }.unwrap();
    setup.outcome.set(2);
    unsafe { session.initialize(now + Duration::from_secs(60), &setup) }.unwrap();
    assert_eq!(setup.attempts.get(), 2);
    session.invalidate();
    unsafe { session.initialize(now, &setup) }.unwrap();
    let sibling_connection = std::rc::Rc::new(std::cell::Cell::new(true));
    let sibling = CaptureSession::<Backend> {
        state: LifecycleState::Active(Box::new(Backend {
            connected: sibling_connection,
            retired: Default::default(),
        })),
        configuration: None,
        counters: Default::default(),
    };
    setup.connected.set(false);
    unsafe { session.initialize(now + Duration::from_secs(60), &setup) }.unwrap();
    assert!(session.is_parked());
    assert_eq!(setup.retired.get(), 1);
    assert_eq!(setup.attempts.get(), 3);
    assert!(sibling.as_ref().unwrap().connected());
    session.invalidate();
    setup.connected.set(true);
    unsafe { session.initialize(now, &setup) }.unwrap();
    session.invalidate();
    assert_eq!(setup.retired.get(), 2);
}
