use parking_lot::{Condvar, Mutex};
use std::sync::Arc;

/// A continuation over a typed hook chain.
pub trait HookChain<'a, T: 'a> {
    /// Advances to the next registered hook, if one remains.
    fn fp_next(&mut self) -> Option<&'a T>;
}

/// Owns the backend resources behind an installed terminal callable.
///
/// Implementations must publish a usable trampoline before `enable` can route
/// calls to the generated hook entry point. `disable` is invoked only after
/// all terminal invocations acquired through the corresponding slot drain.
pub trait TerminalLifecycle: Send + Sync + 'static {
    fn enable(&self) -> Result<(), i32>;
    fn disable(&self) -> Result<(), i32>;
}

/// Supplies the typed callable owned by an installed terminal lifecycle.
pub trait TerminalCallable: TerminalLifecycle {
    type Function: Copy + Send + Sync + 'static;

    fn terminal_callable(&self) -> Self::Function;
}

/// Constructs one owned detour for a generated terminal entry point.
pub trait TerminalDetour<F>: TerminalCallable<Function = F> + Sized {
    fn create(target: F, detour: F) -> Self;
}

/// Resolves a callable from a lifecycle owner on each terminal invocation.
pub struct SelectedTerminal<T, S> {
    lifecycle: T,
    selector: S,
}

impl<T, S> SelectedTerminal<T, S> {
    pub fn new(lifecycle: T, selector: S) -> Self {
        Self {
            lifecycle,
            selector,
        }
    }
}

impl<T: TerminalLifecycle, S: Send + Sync + 'static> TerminalLifecycle for SelectedTerminal<T, S> {
    fn enable(&self) -> Result<(), i32> {
        self.lifecycle.enable()
    }

    fn disable(&self) -> Result<(), i32> {
        self.lifecycle.disable()
    }
}

impl<T, S, F> TerminalCallable for SelectedTerminal<T, S>
where
    T: TerminalLifecycle,
    S: Fn(&T) -> F + Send + Sync + 'static,
    F: Copy + Send + Sync + 'static,
{
    type Function = F;

    fn terminal_callable(&self) -> F {
        (self.selector)(&self.lifecycle)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminalPhase {
    Installed,
    Draining,
}

struct TerminalEntry<T: ?Sized> {
    generation: u64,
    phase: TerminalPhase,
    active: usize,
    terminal: Arc<T>,
}

struct TerminalSlotState<T: ?Sized> {
    next_generation: u64,
    entry: Option<TerminalEntry<T>>,
}

/// Replaceable storage for one generated terminal lifecycle object.
///
/// An uninstall waits for outstanding [`TerminalCall`] leases. It must not be
/// initiated from the same thread while that thread owns a lease from this
/// slot, because doing so would wait on itself.
pub struct TerminalSlot<T: ?Sized> {
    state: Mutex<TerminalSlotState<T>>,
    drained: Condvar,
}

impl<T: ?Sized> TerminalSlot<T> {
    pub const fn new() -> Self {
        Self {
            state: Mutex::new(TerminalSlotState {
                next_generation: 1,
                entry: None,
            }),
            drained: Condvar::new(),
        }
    }

    /// Acquires one invocation lease, or returns `None` while the terminal is
    /// absent or being uninstalled. Generated dispatchers translate `None`
    /// into the terminal declaration's explicit fallback value.
    pub fn acquire(&'static self) -> Option<TerminalCall<T>> {
        let mut state = self.state.lock();
        let entry = state.entry.as_mut()?;
        if entry.phase != TerminalPhase::Installed {
            return None;
        }
        entry.active += 1;
        Some(TerminalCall {
            slot: self,
            generation: entry.generation,
            terminal: Arc::clone(&entry.terminal),
        })
    }
}

impl<T: TerminalLifecycle + ?Sized> TerminalSlot<T> {
    /// Publishes and enables a new terminal generation.
    pub fn install(
        &'static self,
        terminal: Arc<T>,
    ) -> Result<TerminalInstallation<T>, TerminalInstallError> {
        self.retry_failed_cleanup();
        let generation = {
            let mut state = self.state.lock();
            if state.entry.is_some() {
                return Err(TerminalInstallError::Occupied);
            }
            let generation = state.next_generation;
            state.next_generation = state.next_generation.wrapping_add(1).max(1);
            state.entry = Some(TerminalEntry {
                generation,
                phase: TerminalPhase::Installed,
                active: 0,
                terminal: Arc::clone(&terminal),
            });
            generation
        };

        if let Err(error) = terminal.enable() {
            self.remove_failed_install(generation, &terminal);
            return Err(TerminalInstallError::Enable(error));
        }

        Ok(TerminalInstallation {
            slot: self,
            generation,
        })
    }

    fn remove_failed_install(&'static self, generation: u64, terminal: &Arc<T>) {
        let mut state = self.state.lock();
        let Some(entry) = state
            .entry
            .as_mut()
            .filter(|entry| entry.generation == generation)
        else {
            return;
        };
        entry.phase = TerminalPhase::Draining;
        while state.entry.as_ref().is_some_and(|entry| entry.active != 0) {
            self.drained.wait(&mut state);
        }
        drop(state);
        let disabled = terminal.disable().is_ok();
        let mut state = self.state.lock();
        if disabled
            && state
                .entry
                .as_ref()
                .is_some_and(|entry| entry.generation == generation)
        {
            state.entry = None;
        }
    }

    fn retry_failed_cleanup(&'static self) {
        let candidate = {
            let state = self.state.lock();
            state.entry.as_ref().and_then(|entry| {
                (entry.phase == TerminalPhase::Draining && entry.active == 0)
                    .then(|| (entry.generation, Arc::clone(&entry.terminal)))
            })
        };
        let Some((generation, terminal)) = candidate else {
            return;
        };
        if terminal.disable().is_err() {
            return;
        }
        let mut state = self.state.lock();
        if state.entry.as_ref().is_some_and(|entry| {
            entry.generation == generation && entry.phase == TerminalPhase::Draining
        }) {
            state.entry = None;
        }
    }

    fn uninstall(&'static self, generation: u64) -> Result<(), TerminalUninstallError> {
        let terminal = {
            let mut state = self.state.lock();
            let Some(entry) = state.entry.as_mut() else {
                return Err(TerminalUninstallError::Stale);
            };
            if entry.generation != generation || entry.phase != TerminalPhase::Installed {
                return Err(TerminalUninstallError::Stale);
            }
            entry.phase = TerminalPhase::Draining;
            while state.entry.as_ref().is_some_and(|entry| entry.active != 0) {
                self.drained.wait(&mut state);
            }
            Arc::clone(
                &state
                    .entry
                    .as_ref()
                    .expect("terminal entry exists")
                    .terminal,
            )
        };

        if let Err(error) = terminal.disable() {
            let mut state = self.state.lock();
            if let Some(entry) = state
                .entry
                .as_mut()
                .filter(|entry| entry.generation == generation)
            {
                entry.phase = TerminalPhase::Installed;
            }
            return Err(TerminalUninstallError::Disable(error));
        }

        let mut state = self.state.lock();
        if state
            .entry
            .as_ref()
            .is_some_and(|entry| entry.generation == generation)
        {
            state.entry = None;
        }
        Ok(())
    }
}

impl<T: ?Sized> Default for TerminalSlot<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// A lease keeping one terminal generation alive for an entire invocation.
pub struct TerminalCall<T: ?Sized + 'static> {
    slot: &'static TerminalSlot<T>,
    generation: u64,
    terminal: Arc<T>,
}

impl<T: ?Sized> TerminalCall<T> {
    pub fn terminal(&self) -> &T {
        &self.terminal
    }
}

impl<T: ?Sized> Drop for TerminalCall<T> {
    fn drop(&mut self) {
        let mut state = self.slot.state.lock();
        let Some(entry) = state
            .entry
            .as_mut()
            .filter(|entry| entry.generation == self.generation)
        else {
            return;
        };
        entry.active = entry
            .active
            .checked_sub(1)
            .expect("terminal lease underflow");
        if entry.active == 0 && entry.phase == TerminalPhase::Draining {
            self.slot.drained.notify_all();
        }
    }
}

/// Authority to uninstall one exact terminal generation.
#[must_use = "dropping the installation immediately uninstalls its terminal"]
pub struct TerminalInstallation<T: TerminalLifecycle + ?Sized + 'static> {
    slot: &'static TerminalSlot<T>,
    generation: u64,
}

impl<T: TerminalLifecycle + ?Sized> Drop for TerminalInstallation<T> {
    fn drop(&mut self) {
        let _ = self.slot.uninstall(self.generation);
    }
}

impl<T: TerminalLifecycle + ?Sized> TerminalInstallation<T> {
    pub fn uninstall(&self) -> Result<(), TerminalUninstallError> {
        self.slot.uninstall(self.generation)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalInstallError {
    Occupied,
    Enable(i32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalUninstallError {
    Stale,
    Disable(i32),
}

#[doc(hidden)]
pub trait ErasedTerminalInstallation: Send {
    fn uninstall(&self) -> Result<(), TerminalUninstallError>;
}

impl<T: TerminalLifecycle + ?Sized> ErasedTerminalInstallation for TerminalInstallation<T> {
    fn uninstall(&self) -> Result<(), TerminalUninstallError> {
        self.uninstall()
    }
}
