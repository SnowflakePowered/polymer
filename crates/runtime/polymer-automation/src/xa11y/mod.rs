mod snapshot;
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

use crate::{
    AutomationDriver, AutomationError, AutomationRequest, AutomationResponse, Bounds, Diagnosis,
    InputCommand, LocatorAction, MessageBoxes, NodeSnapshot, TreeOptions, UiEvent, WaitCondition,
    WindowId, WindowMatcher, WindowQuery, WindowSnapshot,
};
use xa11y_core::{
    ElementData, Error, EventKind, Locator, Provider, RecvStatus, Role, SelectorGroup, StateFlag,
};
/// Native window discovery and input; selectors and actions remain shared.
pub trait AutomationBackend: Send + 'static {
    fn provider(&mut self) -> Result<Arc<dyn Provider>, AutomationError>;
    fn windows(&mut self) -> Result<Vec<WindowSnapshot>, AutomationError>;
    fn window(&mut self, id: &WindowId) -> Result<(WindowSnapshot, ElementData), AutomationError>;
    fn input(&mut self, id: &WindowId, command: InputCommand) -> Result<(), AutomationError>;
    fn reset(&mut self);
    fn shutdown(&mut self) {
        self.reset();
    }
}

struct EventStream {
    receiver: mpsc::Receiver<UiEvent>,
    cancel: Arc<AtomicBool>,
    overflow: Arc<AtomicBool>,
}

impl Drop for EventStream {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

pub struct Xa11yDriver<B: AutomationBackend> {
    backend: B,
    provider: Option<Arc<dyn Provider>>,
    subscriptions: HashMap<u64, EventStream>,
    next_subscription: u64,
    message_box_cancel: Arc<AtomicBool>,
}

impl<B: AutomationBackend> Xa11yDriver<B> {
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            provider: None,
            subscriptions: HashMap::new(),
            next_subscription: 1,
            message_box_cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    fn windows(&mut self, query: &WindowQuery) -> Result<Vec<WindowSnapshot>, AutomationError> {
        let matcher = query.matcher()?;
        if let Some(selector) = &query.selector {
            SelectorGroup::parse(selector).map_err(Self::map_error)?;
        }
        self.matching_windows(&matcher, query.selector.as_deref())
    }

    fn matching_windows(
        &mut self,
        matcher: &WindowMatcher,
        selector: Option<&str>,
    ) -> Result<Vec<WindowSnapshot>, AutomationError> {
        let candidates = self
            .backend
            .windows()?
            .into_iter()
            .filter(|record| matcher.matches(record))
            .collect::<Vec<_>>();
        let Some(selector) = selector else {
            return Ok(candidates);
        };

        self.ensure_provider()?;
        let provider = self
            .provider
            .as_ref()
            .expect("provider was attached")
            .clone();
        let mut matches = Vec::new();
        for record in candidates {
            let (_, root) = self.backend.window(&record.id)?;
            if Locator::new(provider.clone(), Some(root), selector)
                .exists()
                .map_err(Self::map_error)?
            {
                matches.push(record);
            }
        }
        Ok(matches)
    }

    fn wait_for_window(
        &mut self,
        query: &WindowQuery,
        timeout: Duration,
    ) -> Result<WindowSnapshot, AutomationError> {
        let matcher = query.matcher()?;
        if let Some(selector) = &query.selector {
            SelectorGroup::parse(selector).map_err(Self::map_error)?;
        }
        let started = std::time::Instant::now();
        loop {
            let matches = match self.matching_windows(&matcher, query.selector.as_deref()) {
                Ok(matches) => matches,
                Err(error) if error.kind == "platform" && started.elapsed() < timeout => {
                    // Qt can invalidate its UIA provider while a settings
                    // window is being destroyed. Recreate the client and retry
                    // instead of permanently failing the next window query.
                    self.provider = None;
                    self.backend.reset();
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                Err(error) => return Err(error),
            };
            match matches.as_slice() {
                [window] => return Ok(window.clone()),
                [] if started.elapsed() < timeout => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                [] => {
                    return Err(AutomationError {
                        kind: "windowNotFound".to_owned(),
                        message: format!("no emulator window matched after {timeout:?}"),
                        diagnosis: Some(Box::new(Diagnosis {
                            selector: query.selector.clone(),
                            condition: Some("window matching query".to_owned()),
                            last_observed: Some("no matching windows".to_owned()),
                            ..Diagnosis::default()
                        })),
                    });
                }
                windows => {
                    return Err(AutomationError {
                        kind: "ambiguousWindow".to_owned(),
                        message: format!("window query matched {} windows", windows.len()),
                        diagnosis: Some(Box::new(Diagnosis {
                            candidates: windows
                                .iter()
                                .take(10)
                                .map(|window| format!("{} ({})", window.title, window.class_name))
                                .collect(),
                            ..Diagnosis::default()
                        })),
                    });
                }
            }
        }
    }

    fn window(&mut self, id: &WindowId) -> Result<(WindowSnapshot, ElementData), AutomationError> {
        self.ensure_provider()?;
        self.backend.window(id)
    }

    fn ensure_provider(&mut self) -> Result<(), AutomationError> {
        if self.provider.is_none() {
            self.provider = Some(self.backend.provider()?);
        }
        Ok(())
    }

    fn locator(
        &mut self,
        window: &WindowId,
        selector: &str,
        nth: Option<usize>,
        timeout: Duration,
    ) -> Result<Locator, AutomationError> {
        let (_, root) = self.window(window)?;
        let provider: Arc<dyn Provider> = self
            .provider
            .as_ref()
            .expect("window resolution attached the provider")
            .clone();
        let locator = Locator::new(provider, Some(root), selector).with_timeout(timeout);
        if let Some(nth) = nth {
            if nth == 0 {
                return Err(AutomationError::invalid_argument("locator nth is 1-based"));
            }
            return Ok(locator.nth(nth));
        }
        Ok(locator)
    }

    fn unique_locator(
        &mut self,
        window: &WindowId,
        selector: &str,
        nth: Option<usize>,
        timeout: Duration,
    ) -> Result<Locator, AutomationError> {
        let locator = self.locator(window, selector, nth, timeout)?;
        let count = nth
            .is_none()
            .then(|| locator.count().map_err(Self::map_error))
            .transpose()?
            .unwrap_or_default();
        if count > 1 {
            return Err(AutomationError {
                kind: "ambiguousSelector".to_owned(),
                message: format!("selector '{selector}' matched {count} elements"),
                diagnosis: Some(Box::new(Diagnosis {
                    selector: Some(selector.to_owned()),
                    last_observed: Some(format!("{count} matching elements")),
                    ..Diagnosis::default()
                })),
            });
        }
        Ok(locator)
    }
}

impl<B: AutomationBackend> AutomationDriver for Xa11yDriver<B> {
    fn execute(
        &mut self,
        request: AutomationRequest,
    ) -> Result<AutomationResponse, AutomationError> {
        match request {
            AutomationRequest::Windows { query } => {
                self.windows(&query).map(AutomationResponse::Windows)
            }
            AutomationRequest::WaitForWindow { query, timeout_ms } => self
                .wait_for_window(&query, Duration::from_millis(timeout_ms))
                .map(AutomationResponse::Window),
            AutomationRequest::WindowSnapshot { window } => self
                .window(&window)
                .map(|(snapshot, _)| AutomationResponse::Window(snapshot)),
            AutomationRequest::Tree { window, options } => {
                let (_, root) = self.window(&window)?;
                let (root, truncated) = self.tree(&root, options)?;
                Ok(AutomationResponse::Tree { root, truncated })
            }
            AutomationRequest::LocatorCount { window, selector } => {
                let locator = self.locator(&window, &selector, None, Duration::ZERO)?;
                locator
                    .count()
                    .map(AutomationResponse::Count)
                    .map_err(Self::map_error)
            }
            AutomationRequest::LocatorAll {
                window,
                selector,
                limit,
            } => {
                if limit == 0 || limit > 1_000 {
                    return Err(AutomationError::invalid_argument(
                        "locator result limit must be between 1 and 1000",
                    ));
                }
                let locator = self.locator(&window, &selector, None, Duration::ZERO)?;
                locator
                    .elements()
                    .map(|elements| {
                        AutomationResponse::Nodes(
                            elements
                                .iter()
                                .take(limit)
                                .map(|element| Self::node(element.data()))
                                .collect(),
                        )
                    })
                    .map_err(Self::map_error)
            }
            AutomationRequest::LocatorWait {
                window,
                selector,
                nth,
                condition,
                timeout_ms,
            } => {
                let timeout = Duration::from_millis(timeout_ms);
                let locator = self.unique_locator(&window, &selector, nth, timeout)?;
                Self::wait(&locator, condition, timeout)
                    .map(|node| node.map_or(AutomationResponse::Unit, AutomationResponse::Node))
            }
            AutomationRequest::LocatorAction {
                window,
                selector,
                nth,
                action,
                timeout_ms,
            } => {
                let locator = self.unique_locator(
                    &window,
                    &selector,
                    nth,
                    Duration::from_millis(timeout_ms),
                )?;
                Self::perform(&locator, action).map(|()| AutomationResponse::Unit)
            }
            AutomationRequest::Input { window, command } => {
                self.window(&window)?;
                self.backend
                    .input(&window, command)
                    .map(|()| AutomationResponse::Unit)
            }
            AutomationRequest::Subscribe { window, filter } => {
                let (_, root) = self.window(&window)?;
                let subscription = self
                    .provider
                    .as_ref()
                    .expect("window resolution attached the provider")
                    .subscribe(&root)
                    .map_err(Self::map_error)?;
                let (events, receiver) = mpsc::sync_channel(256);
                let cancel = Arc::new(AtomicBool::new(false));
                let overflow = Arc::new(AtomicBool::new(false));
                let worker_cancel = cancel.clone();
                let worker_overflow = overflow.clone();
                std::thread::Builder::new()
                    .name("polymer-ui-events".to_owned())
                    .spawn(move || {
                        while !worker_cancel.load(Ordering::Acquire) {
                            match subscription.recv_status(Duration::from_millis(100)) {
                                RecvStatus::Event(event) => {
                                    let event = Self::event(*event);
                                    if !filter.types.is_empty()
                                        && !filter.types.contains(&event.event_type)
                                    {
                                        continue;
                                    }
                                    match events.try_send(event) {
                                        Ok(()) => {}
                                        Err(mpsc::TrySendError::Full(_)) => {
                                            worker_overflow.store(true, Ordering::Release);
                                            break;
                                        }
                                        Err(mpsc::TrySendError::Disconnected(_)) => break,
                                    }
                                }
                                RecvStatus::Timeout => {}
                                RecvStatus::Disconnected => break,
                            }
                        }
                    })
                    .map_err(|error| AutomationError::platform(error.to_string()))?;
                let id = self.next_subscription;
                self.next_subscription += 1;
                self.subscriptions.insert(
                    id,
                    EventStream {
                        receiver,
                        cancel,
                        overflow,
                    },
                );
                Ok(AutomationResponse::Subscription(id))
            }
            AutomationRequest::NextEvent {
                subscription,
                timeout_ms,
            } => {
                let stream = self.subscriptions.get(&subscription).ok_or_else(|| {
                    AutomationError::new("staleSubscription", "event subscription is closed")
                })?;
                let timeout = Duration::from_millis(timeout_ms);
                match stream.receiver.recv_timeout(timeout) {
                    Ok(event) => Ok(AutomationResponse::Event(Some(event))),
                    Err(mpsc::RecvTimeoutError::Timeout) => Ok(AutomationResponse::Event(None)),
                    Err(mpsc::RecvTimeoutError::Disconnected)
                        if stream.overflow.load(Ordering::Acquire) =>
                    {
                        Err(AutomationError::new(
                            "eventOverflow",
                            "accessibility event queue exceeded 256 entries",
                        ))
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => Err(AutomationError::new(
                        "staleSubscription",
                        "accessibility event source disconnected",
                    )),
                }
            }
            AutomationRequest::CloseEvents { subscription } => {
                self.subscriptions.remove(&subscription);
                Ok(AutomationResponse::Unit)
            }
        }
    }

    fn message_boxes(&mut self) -> Result<MessageBoxes, AutomationError> {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut processes = HashSet::new();
        for window in self.backend.windows()? {
            if !processes.insert(window.process_id) {
                continue;
            }
            let (_, root) = self.window(&window.id)?;
            let subscription = self
                .provider
                .as_ref()
                .expect("window resolution attached the provider")
                .subscribe(&root)
                .map_err(Self::map_error)?;
            let sender = sender.clone();
            let cancel = self.message_box_cancel.clone();
            std::thread::Builder::new()
                .name("polymer-ui-message-boxes".to_owned())
                .spawn(move || {
                    while !cancel.load(Ordering::Acquire) && !sender.is_closed() {
                        match subscription.recv_status(Duration::from_secs(1)) {
                            RecvStatus::Event(event) => {
                                let Some(target) = event.target.filter(|target| {
                                    matches!(event.kind, EventKind::WindowOpened)
                                        && (target.role == Role::Dialog || target.states.modal)
                                }) else {
                                    continue;
                                };
                                if sender.send(Self::node(&target)).is_err() {
                                    break;
                                }
                            }
                            RecvStatus::Timeout => {}
                            RecvStatus::Disconnected => break,
                        }
                    }
                })
                .map_err(|error| AutomationError::platform(error.to_string()))?;
        }
        if processes.is_empty() {
            return Err(AutomationError::new(
                "windowNotFound",
                "the emulator has no windows to watch for message boxes",
            ));
        }
        Ok(MessageBoxes(receiver))
    }

    fn shutdown(&mut self) {
        self.message_box_cancel.store(true, Ordering::Release);
        self.subscriptions.clear();
        self.provider = None;
        self.backend.shutdown();
    }
}
