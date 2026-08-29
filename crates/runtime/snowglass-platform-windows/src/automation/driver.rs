use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

use snowglass_automation::{
    AutomationDriver, AutomationError, AutomationRequest, AutomationResponse, Bounds, Diagnosis,
    LocatorAction, NodeSnapshot, TreeOptions, UiEvent, WaitCondition, WindowId, WindowQuery,
    WindowSnapshot,
};
use xa11y_core::{ElementData, Error, EventKind, Locator, Provider, RecvStatus, StateFlag};
use xa11y_windows::WindowsProvider;

use super::{catalog::WindowCatalog, input::TargetedWindowInput};
use crate::{DesktopThreadAttachment, PrivateDesktop, ProcessId};

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

pub struct WindowsAutomationDriver {
    provider: Option<Arc<WindowsProvider>>,
    desktop_name: Option<String>,
    desktop_attachment: Option<DesktopThreadAttachment>,
    desktop: Option<PrivateDesktop>,
    catalog: WindowCatalog,
    subscriptions: HashMap<u64, EventStream>,
    next_subscription: u64,
}

impl WindowsAutomationDriver {
    pub fn new(
        root_process_id: u32,
        desktop_name: Option<String>,
    ) -> Result<Self, AutomationError> {
        Ok(Self {
            provider: None,
            desktop_name,
            desktop_attachment: None,
            desktop: None,
            catalog: WindowCatalog::new(ProcessId(root_process_id)),
            subscriptions: HashMap::new(),
            next_subscription: 1,
        })
    }

    fn windows(&mut self, query: &WindowQuery) -> Result<Vec<WindowSnapshot>, AutomationError> {
        query.validate()?;
        self.ensure_desktop()?;
        Ok(self
            .catalog
            .enumerate()?
            .into_iter()
            .map(|record| record.snapshot)
            .filter(|window| query.matches(window))
            .collect())
    }

    fn wait_for_window(
        &mut self,
        query: &WindowQuery,
        timeout: Duration,
    ) -> Result<WindowSnapshot, AutomationError> {
        let started = std::time::Instant::now();
        loop {
            let matches = self.windows(query)?;
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

    fn window(
        &mut self,
        id: &WindowId,
    ) -> Result<(isize, WindowSnapshot, ElementData), AutomationError> {
        let record = self
            .catalog
            .enumerate()?
            .into_iter()
            .find(|record| record.snapshot.id == *id)
            .ok_or_else(|| {
                AutomationError::new(
                    "staleWindow",
                    format!("window '{}' is no longer available", id.0),
                )
            })?;
        self.ensure_provider()?;
        let provider = self.provider.as_ref().expect("provider was attached");
        let root = provider
            .element_from_native_window(record.hwnd)
            .map_err(Self::map_error)?;
        Ok((record.hwnd, record.snapshot, root))
    }

    fn ensure_provider(&mut self) -> Result<(), AutomationError> {
        if self.provider.is_some() {
            return Ok(());
        }
        self.ensure_desktop()?;
        self.provider = Some(Arc::new(WindowsProvider::new().map_err(Self::map_error)?));
        Ok(())
    }

    fn ensure_desktop(&mut self) -> Result<(), AutomationError> {
        if self.desktop.is_some() || self.desktop_name.is_none() {
            return Ok(());
        }
        if let Some(name) = &self.desktop_name {
            let desktop = PrivateDesktop::open(name)
                .map_err(|error| AutomationError::platform(error.to_string()))?;
            let attachment = desktop
                .attach_current_thread()
                .map_err(|error| AutomationError::platform(error.to_string()))?;
            self.desktop = Some(desktop);
            self.desktop_attachment = Some(attachment);
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
        let (_, _, root) = self.window(window)?;
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

    fn node(element: &ElementData) -> NodeSnapshot {
        NodeSnapshot {
            role: element.role.to_snake_case().to_owned(),
            name: element.name.clone(),
            value: element.value.clone(),
            description: element.description.clone(),
            bounds: element.bounds.map(|bounds| Bounds {
                x: f64::from(bounds.x),
                y: f64::from(bounds.y),
                width: f64::from(bounds.width),
                height: f64::from(bounds.height),
            }),
            actions: element.actions.clone(),
            states: serde_json::to_value(&element.states).unwrap_or(serde_json::Value::Null),
            numeric_value: element.numeric_value,
            min_value: element.min_value,
            max_value: element.max_value,
            stable_id: element.stable_id.clone(),
            process_id: element.pid,
            raw: serde_json::to_value(&element.raw).unwrap_or(serde_json::Value::Null),
            children: Vec::new(),
        }
    }

    fn tree(
        &self,
        root: &ElementData,
        options: TreeOptions,
    ) -> Result<(NodeSnapshot, bool), AutomationError> {
        let options = options.validate()?;
        let mut remaining = options.max_nodes;
        let mut truncated = false;
        let root = self.tree_node(root, 0, options.max_depth, &mut remaining, &mut truncated)?;
        Ok((root, truncated))
    }

    fn tree_node(
        &self,
        element: &ElementData,
        depth: usize,
        max_depth: usize,
        remaining: &mut usize,
        truncated: &mut bool,
    ) -> Result<NodeSnapshot, AutomationError> {
        *remaining = remaining.saturating_sub(1);
        let mut node = Self::node(element);
        if depth >= max_depth {
            if !self
                .provider
                .as_ref()
                .expect("tree roots require an attached provider")
                .get_children(Some(element))
                .map_err(Self::map_error)?
                .is_empty()
            {
                *truncated = true;
            }
            return Ok(node);
        }
        for child in self
            .provider
            .as_ref()
            .expect("tree roots require an attached provider")
            .get_children(Some(element))
            .map_err(Self::map_error)?
        {
            if *remaining == 0 {
                *truncated = true;
                break;
            }
            node.children.push(self.tree_node(
                &child,
                depth + 1,
                max_depth,
                remaining,
                truncated,
            )?);
        }
        Ok(node)
    }

    fn perform(locator: &Locator, action: LocatorAction) -> Result<(), AutomationError> {
        let result = match action {
            LocatorAction::Press => locator.press(),
            LocatorAction::Focus => locator.focus(),
            LocatorAction::Blur => locator.blur(),
            LocatorAction::Toggle => locator.toggle(),
            LocatorAction::Select => locator.select(),
            LocatorAction::Expand => locator.expand(),
            LocatorAction::Collapse => locator.collapse(),
            LocatorAction::ShowMenu => locator.show_menu(),
            LocatorAction::Increment => locator.increment(),
            LocatorAction::Decrement => locator.decrement(),
            LocatorAction::ScrollIntoView => locator.scroll_into_view(),
            LocatorAction::SetValue(value) => locator.set_value(&value),
            LocatorAction::SetNumericValue(value) => locator.set_numeric_value(value),
            LocatorAction::TypeText(value) => locator.type_text(&value),
            LocatorAction::SetTextSelection { start, end } => locator.select_text(start, end),
            LocatorAction::Perform(action) => locator.perform_action(&action),
        };
        result.map_err(Self::map_error)
    }

    fn wait(
        locator: &Locator,
        condition: WaitCondition,
        timeout: Duration,
    ) -> Result<Option<NodeSnapshot>, AutomationError> {
        match condition {
            WaitCondition::Attached => locator
                .wait_attached(timeout)
                .map(|element| Some(Self::node(element.data()))),
            WaitCondition::Detached => locator.wait_detached(timeout).map(|()| None),
            WaitCondition::Visible => locator
                .wait_visible(timeout)
                .map(|element| Some(Self::node(element.data()))),
            WaitCondition::Hidden => locator.wait_hidden(timeout).map(|()| None),
            WaitCondition::Enabled => locator
                .wait_enabled(timeout)
                .map(|element| Some(Self::node(element.data()))),
            WaitCondition::Focused => locator
                .wait_focused(timeout)
                .map(|element| Some(Self::node(element.data()))),
        }
        .map_err(Self::map_error)
    }

    fn event(event: xa11y_core::Event) -> UiEvent {
        let (event_type, state, value) = match event.kind {
            EventKind::FocusChanged => ("focusChanged", None, None),
            EventKind::ValueChanged => ("valueChanged", None, None),
            EventKind::NameChanged => ("nameChanged", None, None),
            EventKind::StateChanged { flag, value } => (
                "stateChanged",
                Some(Self::state_name(flag).to_owned()),
                Some(value),
            ),
            EventKind::StructureChanged => ("structureChanged", None, None),
            EventKind::WindowOpened => ("windowOpened", None, None),
            EventKind::WindowClosed => ("windowClosed", None, None),
            EventKind::WindowActivated => ("windowActivated", None, None),
            EventKind::WindowDeactivated => ("windowDeactivated", None, None),
            EventKind::SelectionChanged => ("selectionChanged", None, None),
            EventKind::MenuOpened => ("menuOpened", None, None),
            EventKind::MenuClosed => ("menuClosed", None, None),
            EventKind::TextChanged => ("textChanged", None, None),
            EventKind::Announcement => ("announcement", None, None),
            _ => ("unknown", None, None),
        };
        UiEvent {
            event_type: event_type.to_owned(),
            target: event.target.as_ref().map(Self::node),
            state,
            value,
        }
    }

    fn state_name(flag: StateFlag) -> &'static str {
        match flag {
            StateFlag::Enabled => "enabled",
            StateFlag::Visible => "visible",
            StateFlag::Focused => "focused",
            StateFlag::Checked => "checked",
            StateFlag::Selected => "selected",
            StateFlag::Expanded => "expanded",
            StateFlag::Editable => "editable",
            StateFlag::Focusable => "focusable",
            StateFlag::Modal => "modal",
            StateFlag::Required => "required",
            StateFlag::Busy => "busy",
            _ => "unknown",
        }
    }

    fn map_error(error: Error) -> AutomationError {
        let (kind, diagnosis) = match &error {
            Error::SelectorNotMatched { diagnosis, .. } => (
                "selectorNotMatched",
                diagnosis.as_deref().map(Self::diagnosis).map(Box::new),
            ),
            Error::Timeout { diagnosis, .. } => (
                "timeout",
                diagnosis.as_deref().map(Self::diagnosis).map(Box::new),
            ),
            Error::ElementStale { .. } => ("staleElement", None),
            Error::ActionNotSupported { .. } | Error::TextValueNotSupported => {
                ("actionNotSupported", None)
            }
            Error::InvalidSelector { .. } | Error::InvalidActionData { .. } => {
                ("invalidArgument", None)
            }
            Error::PermissionDenied { .. } => ("permissionDenied", None),
            Error::Unsupported { .. } => ("unsupported", None),
            Error::Platform { .. } => ("platform", None),
            _ => ("accessibility", None),
        };
        AutomationError {
            kind: kind.to_owned(),
            message: error.to_string(),
            diagnosis,
        }
    }

    fn diagnosis(value: &xa11y_core::Diagnosis) -> Diagnosis {
        Diagnosis {
            selector: value.selector.clone(),
            condition: value.condition.clone(),
            last_observed: value.last_observed.clone(),
            candidates: value.candidates.clone(),
            scope: value.scope.clone(),
        }
    }
}

impl AutomationDriver for WindowsAutomationDriver {
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
                .map(|(_, snapshot, _)| AutomationResponse::Window(snapshot)),
            AutomationRequest::Tree { window, options } => {
                let (_, _, root) = self.window(&window)?;
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
                let (native, _, _) = self.window(&window)?;
                TargetedWindowInput::new(native)
                    .execute(command)
                    .map(|()| AutomationResponse::Unit)
            }
            AutomationRequest::Subscribe { window, filter } => {
                let (_, _, root) = self.window(&window)?;
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
                    .name("snowglass-ui-events".to_owned())
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

    fn shutdown(&mut self) {
        self.subscriptions.clear();
        self.provider = None;
        self.desktop_attachment = None;
        self.desktop = None;
    }
}
