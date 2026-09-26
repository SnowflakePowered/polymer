mod input;

use polymer_automation::{
    AutomationBackend, AutomationDriver, AutomationError, AutomationRequest, AutomationResponse,
    InputCommand, MessageBoxes, WindowId, WindowSnapshot, Xa11yDriver,
};
use std::{collections::HashMap, sync::Arc};
use xa11y_core::{ElementData, Provider, Role};
use xa11y_macos::MacOSProvider;

struct MacAutomationBackend {
    pid: u32,
    input: polymer_ipc::input::host::InputIpcListener,
    provider: Option<Arc<MacOSProvider>>,
    windows: HashMap<String, ElementData>,
}

pub struct MacAutomationDriver(Xa11yDriver<MacAutomationBackend>);

impl MacAutomationBackend {
    fn snapshot(&self, root: &ElementData) -> WindowSnapshot {
        WindowSnapshot {
            id: WindowId(format!("{}:{}", self.pid, root.handle)),
            process_id: self.pid,
            title: root.name.clone().unwrap_or_default(),
            class_name: root.role.to_snake_case().to_owned(),
            visible: root.states.visible,
        }
    }
}

impl AutomationBackend for MacAutomationBackend {
    fn provider(&mut self) -> Result<Arc<dyn Provider>, AutomationError> {
        if self.provider.is_none() {
            self.provider = Some(Arc::new(
                MacOSProvider::new().map_err(MacAutomationDriver::map_error)?,
            ));
        }
        Ok(self.provider.as_ref().unwrap().clone())
    }

    fn windows(&mut self) -> Result<Vec<WindowSnapshot>, AutomationError> {
        let provider = self.provider()?;
        let app = match provider.app_by_pid(self.pid) {
            Ok(app) => app,
            Err(xa11y_core::Error::SelectorNotMatched { .. }) => return Ok(Vec::new()),
            Err(error) => return Err(MacAutomationDriver::map_error(error)),
        };
        let mut snapshots = Vec::new();
        let mut pending = provider
            .get_children(Some(&app))
            .map_err(MacAutomationDriver::map_error)?;
        while let Some(root) = pending.pop() {
            if !matches!(root.role, Role::Window | Role::Dialog | Role::Menu) {
                continue;
            }
            // Cocoa modal sheets are children of their parent window, not the app.
            pending.extend(
                provider
                    .get_children(Some(&root))
                    .map_err(MacAutomationDriver::map_error)?
                    .into_iter()
                    .filter(|child| matches!(child.role, Role::Dialog | Role::Window)),
            );
            // Qt can expose a popup through AX before AppKit orders its
            // native window, and retain its AX node after it closes.
            // Only publish windows we can actually target on this process.
            match input::TargetedInput::window(self.pid, &root) {
                Ok(_) => {}
                Err(error) if error.kind == "staleWindow" || error.kind == "notInteractable" => {
                    continue;
                }
                Err(error) => return Err(error),
            }
            let snapshot = self.snapshot(&root);
            self.windows.insert(snapshot.id.0.clone(), root);
            snapshots.push(snapshot);
        }
        Ok(snapshots)
    }

    fn window(&mut self, id: &WindowId) -> Result<(WindowSnapshot, ElementData), AutomationError> {
        let root = self
            .windows
            .get(&id.0)
            .ok_or_else(|| AutomationError::new("staleWindow", "window is no longer available"))?
            .clone();
        self.provider()?
            .get_children(Some(&root))
            .map_err(MacAutomationDriver::map_error)?;
        Ok((self.snapshot(&root), root))
    }

    fn input(&mut self, id: &WindowId, command: InputCommand) -> Result<(), AutomationError> {
        let (_, root) = self.window(id)?;
        let provider = self.provider()?;
        input::TargetedInput::execute(&self.input, self.pid, provider.as_ref(), &root, command)
    }

    fn reset(&mut self) {
        self.windows.clear();
        self.provider = None;
    }
}

impl MacAutomationDriver {
    pub fn new(
        pid: u32,
        input: polymer_ipc::input::host::InputIpcListener,
    ) -> Result<Self, AutomationError> {
        Ok(Self(Xa11yDriver::new(MacAutomationBackend {
            pid,
            input,
            provider: None,
            windows: HashMap::new(),
        })))
    }
    fn map_error(error: xa11y_core::Error) -> AutomationError {
        Xa11yDriver::<MacAutomationBackend>::map_error(error)
    }
}
impl AutomationDriver for MacAutomationDriver {
    fn execute(
        &mut self,
        request: AutomationRequest,
    ) -> Result<AutomationResponse, AutomationError> {
        self.0.execute(request)
    }
    fn message_boxes(&mut self) -> Result<MessageBoxes, AutomationError> {
        self.0.message_boxes()
    }
    fn shutdown(&mut self) {
        self.0.shutdown();
    }
}
