use std::collections::HashMap;

use polymer_automation::{AutomationError, WindowId, WindowSnapshot};
use xa11y_core::{ElementData, Provider};
use xa11y_windows::WindowsProvider;

use crate::{ProcessId, process_tree_ids};

use super::driver::WindowsAutomationDriver;

pub(super) struct WindowRecord {
    pub hwnd: isize,
    pub snapshot: WindowSnapshot,
    pub element: ElementData,
}

pub(super) struct WindowCatalog {
    root: ProcessId,
    next_id: u64,
    ids: HashMap<isize, WindowId>,
}

impl WindowCatalog {
    pub fn new(root: ProcessId) -> Self {
        Self {
            root,
            next_id: 1,
            ids: HashMap::new(),
        }
    }

    pub fn enumerate(
        &mut self,
        provider: &WindowsProvider,
    ) -> Result<Vec<WindowRecord>, AutomationError> {
        let processes = process_tree_ids(self.root)
            .map_err(|error| AutomationError::platform(error.to_string()))?;
        if !processes.contains(&self.root) {
            return Err(AutomationError::process_exited());
        }
        let mut windows = Vec::new();
        for process in processes {
            let app = match provider.app_by_pid(process.0) {
                Ok(app) => app,
                Err(xa11y_core::Error::SelectorNotMatched { .. }) => continue,
                Err(error) => return Err(WindowsAutomationDriver::map_error(error)),
            };
            let children = provider
                .app_windows(&app)
                .map_err(WindowsAutomationDriver::map_error)?;
            windows.extend(
                children
                    .into_iter()
                    .filter_map(|element| self.record(element)),
            );
        }
        Ok(windows)
    }

    fn record(&mut self, element: ElementData) -> Option<WindowRecord> {
        let hwnd =
            isize::from_str_radix(element.stable_id.as_deref()?.strip_prefix("hwnd:0x")?, 16)
                .ok()?;
        let id = self.ids.entry(hwnd).or_insert_with(|| {
            let id = WindowId(format!("window-{}", self.next_id));
            self.next_id += 1;
            id
        });
        Some(WindowRecord {
            hwnd,
            snapshot: WindowSnapshot {
                id: id.clone(),
                process_id: element.pid.unwrap_or_default(),
                title: element.name.clone().unwrap_or_default(),
                class_name: element
                    .raw
                    .get("class_name")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_owned(),
                visible: element.states.visible,
            },
            element,
        })
    }
}
