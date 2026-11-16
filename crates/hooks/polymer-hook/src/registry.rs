use crate::terminal::{TerminalInstallError, TerminalUninstallError};
use parking_lot::Mutex;

pub struct HookInstallError<I> {
    pub source: TerminalInstallError,
    pub installation: Option<I>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HookRegistryError {
    Install(TerminalInstallError),
    Uninstall(TerminalUninstallError),
}

#[doc(hidden)]
pub trait ManagedHookInstallation: Sized + Send + 'static {
    fn create() -> Result<Self, HookInstallError<Self>>;
    fn uninstall_terminals(&mut self) -> Result<(), TerminalUninstallError>;
    fn is_complete(&self) -> bool;
    fn is_empty(&self) -> bool;
}

pub struct HookInstallationRegistry<I> {
    installation: Mutex<Option<I>>,
}

impl<I> Default for HookInstallationRegistry<I> {
    fn default() -> Self {
        Self::new()
    }
}

impl<I> HookInstallationRegistry<I> {
    pub const fn new() -> Self {
        Self {
            installation: Mutex::new(None),
        }
    }
}

impl<I: ManagedHookInstallation> HookInstallationRegistry<I> {
    pub fn install(&self) -> Result<(), HookRegistryError> {
        let mut slot = self.installation.lock();
        if let Some(installation) = slot.as_mut() {
            if installation.is_complete() {
                return Ok(());
            }
            installation
                .uninstall_terminals()
                .map_err(HookRegistryError::Uninstall)?;
            *slot = None;
        }
        match I::create() {
            Ok(installation) => {
                *slot = Some(installation);
                Ok(())
            }
            Err(mut failure) => {
                *slot = failure.installation.take();
                Err(HookRegistryError::Install(failure.source))
            }
        }
    }

    pub fn uninstall(&self) -> Result<(), HookRegistryError> {
        let mut slot = self.installation.lock();
        let Some(installation) = slot.as_mut() else {
            return Ok(());
        };
        let result = installation
            .uninstall_terminals()
            .map_err(HookRegistryError::Uninstall);
        if installation.is_empty() {
            *slot = None;
        }
        result
    }
}
