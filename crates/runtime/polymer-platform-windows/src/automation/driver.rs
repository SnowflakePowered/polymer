use super::{catalog::WindowCatalog, input::TargetedWindowInput};
use crate::{DesktopThreadAttachment, PrivateDesktop, ProcessId};
use polymer_automation::{
    AutomationBackend, AutomationDriver, AutomationError, AutomationRequest, AutomationResponse,
    InputCommand, WindowId, WindowSnapshot, Xa11yDriver,
};
use std::sync::Arc;
use xa11y_core::{ElementData, Provider};
use xa11y_windows::WindowsProvider;

struct WindowsAutomationBackend {
    provider: Option<Arc<WindowsProvider>>,
    desktop_name: Option<String>,
    desktop_attachment: Option<DesktopThreadAttachment>,
    desktop: Option<PrivateDesktop>,
    catalog: WindowCatalog,
}
pub struct WindowsAutomationDriver(Xa11yDriver<WindowsAutomationBackend>);
impl WindowsAutomationBackend {
    fn window(
        &mut self,
        id: &WindowId,
    ) -> Result<(isize, WindowSnapshot, ElementData), AutomationError> {
        self.ensure_provider()?;
        let provider = self.provider.as_ref().expect("provider was attached");
        let record = self
            .catalog
            .enumerate(provider)?
            .into_iter()
            .find(|record| record.snapshot.id == *id)
            .ok_or_else(|| {
                AutomationError::new(
                    "staleWindow",
                    format!("window '{}' is no longer available", id.0),
                )
            })?;
        Ok((record.hwnd, record.snapshot, record.element))
    }

    fn ensure_provider(&mut self) -> Result<(), AutomationError> {
        if self.provider.is_some() {
            return Ok(());
        }
        self.ensure_desktop()?;
        self.provider = Some(Arc::new(
            WindowsProvider::new().map_err(WindowsAutomationDriver::map_error)?,
        ));
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
}
impl AutomationBackend for WindowsAutomationBackend {
    fn provider(&mut self) -> Result<Arc<dyn Provider>, AutomationError> {
        self.ensure_provider()?;
        Ok(self.provider.as_ref().unwrap().clone())
    }
    fn windows(&mut self) -> Result<Vec<WindowSnapshot>, AutomationError> {
        self.ensure_provider()?;
        let provider = self.provider.as_ref().expect("provider was attached");
        Ok(self
            .catalog
            .enumerate(provider)?
            .into_iter()
            .map(|r| r.snapshot)
            .collect())
    }
    fn window(&mut self, id: &WindowId) -> Result<(WindowSnapshot, ElementData), AutomationError> {
        let (_, snapshot, root) = self.window(id)?;
        Ok((snapshot, root))
    }
    fn input(&mut self, id: &WindowId, command: InputCommand) -> Result<(), AutomationError> {
        let (native, _, _) = self.window(id)?;
        TargetedWindowInput::new(native).execute(command)
    }
    fn reset(&mut self) {
        self.provider = None;
    }
    fn shutdown(&mut self) {
        self.reset();
        self.desktop_attachment = None;
        self.desktop = None;
    }
}

impl WindowsAutomationDriver {
    pub fn new(
        root_process_id: u32,
        desktop_name: Option<String>,
    ) -> Result<Self, AutomationError> {
        Ok(Self(Xa11yDriver::new(WindowsAutomationBackend {
            provider: None,
            desktop_name,
            desktop_attachment: None,
            desktop: None,
            catalog: WindowCatalog::new(ProcessId(root_process_id)),
        })))
    }
    pub(super) fn map_error(error: xa11y_core::Error) -> AutomationError {
        Xa11yDriver::<WindowsAutomationBackend>::map_error(error)
    }
}
impl AutomationDriver for WindowsAutomationDriver {
    fn execute(
        &mut self,
        request: AutomationRequest,
    ) -> Result<AutomationResponse, AutomationError> {
        self.0.execute(request)
    }
    fn shutdown(&mut self) {
        self.0.shutdown();
    }
}
