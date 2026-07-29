//! Shared typed-hook infrastructure.

pub mod config;
mod vtable;

#[doc(hidden)]
pub use dashmap::DashMap as __DashMap;
#[doc(hidden)]
pub use parking_lot::{Mutex as __Mutex, RwLock as __RwLock};
#[doc(hidden)]
pub use paste::paste as __paste;
pub use vtable::ComProxy;
pub use vtable::VtableHook;
#[doc(hidden)]
pub use windows_core::GUID as __GUID;

use parking_lot::{Condvar, Mutex};
use std::sync::Arc;

/// Provenance-preserving identity key for pointers stored in hook registries.
#[doc(hidden)]
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct __PointerKey(core::ptr::NonNull<core::ffi::c_void>);

impl __PointerKey {
    #[doc(hidden)]
    pub fn new(pointer: *mut core::ffi::c_void) -> Option<Self> {
        core::ptr::NonNull::new(pointer).map(Self)
    }

    #[doc(hidden)]
    pub fn as_ptr(self) -> *mut core::ffi::c_void {
        self.0.as_ptr()
    }
}

// SAFETY: registry keys are used only for pointer identity. They are never
// dereferenced through this wrapper, and moving or sharing a pointer value does
// not grant access to its pointee.
unsafe impl Send for __PointerKey {}
unsafe impl Sync for __PointerKey {}

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

/// Advances a terminal-backed hook chain and invokes its continuation.
///
/// The context is passed to the continuation after the listed ABI arguments.
/// This consumes the context, so clone or copy anything needed after the call
/// before invoking `next!`.
#[macro_export]
macro_rules! next {
    ($context:ident $(, $argument:expr)* $(,)?) => {{
        let continuation = *$crate::HookChain::fp_next(&mut $context)
            .expect("hook chain has no continuation");
        unsafe { continuation($($argument,)* $context) }
    }};
}

/// Declares static typed hook chains using function-shaped Rust syntax.
///
/// Each terminal is declared as `direct = path`, `installed` with a fallback,
/// or a backend-defined `dynamic(...)` source. An installed raw-hook group can
/// select its lifecycle owner with `#![detour(Type)]`; each installed terminal
/// then supplies its own `target` and optional `continuation`. A generated
/// A generated private registry owns transactional installation, idempotence,
/// partial cleanup, and reinstallation. Dynamic sources select
/// their callback with `#![terminal_resolver(name)]`; direct and installed-only
/// declarations need no resolver. Hook and context names are inferred from the
/// declared function name in the same way as [`function_table_hook!`]. Mark an
/// installer with `state = Type` to inject its shared state into every hook
/// context and return the state from `install`. Mark an entry with
/// `#[trampoline]` when its declared function is used as an ABI
/// detour target. `#[trampoline(abi = "...")]` overrides that entry point's ABI
/// while leaving the chain declaration as the source of its signature.
#[doc(hidden)]
#[macro_export]
macro_rules! chain_hooks {
    (
        #![terminal_resolver($resolve:ident)]
        #![order($order:ident)]
        #![detour($detour:ty)]
        #![state($state_type:ty = $state_value:expr)]
        $($items:tt)*
    ) => {
        $crate::chain_hooks!(@items $order, $resolve, [$detour],
            [state: $state_type = $state_value]; $($items)*
        );
    };

    (
        #![order($order:ident)]
        #![detour($detour:ty)]
        #![state($state_type:ty = $state_value:expr)]
        $($items:tt)*
    ) => {
        $crate::chain_hooks!(@items $order, __unused_terminal_resolver, [$detour],
            [state: $state_type = $state_value]; $($items)*
        );
    };

    (
        #![order($order:ident)]
        #![detour($detour:ty)]
        #[installer(state = $state:ty)]
        $installer_visibility:vis struct $installer:ident;
        $($items:tt)*
    ) => { $crate::__paste! {
        $crate::chain_hooks!(@declare_installer
            [$installer_visibility] [$installer] [state $state] [$($items)*]
        );
        impl $crate::ManagedHookInstallation for $installer {
            fn create() -> Result<Self, $crate::HookInstallError<Self>> { Self::create() }
            fn uninstall_terminals(&mut self) -> Result<(), $crate::TerminalUninstallError> {
                self.uninstall_terminals()
            }
            fn is_complete(&self) -> bool { self.is_complete() }
            fn is_empty(&self) -> bool { self.is_empty() }
        }
        #[allow(non_upper_case_globals)]
        static [<__ $installer _registry>]:
            $crate::HookInstallationRegistry<$installer> =
                $crate::HookInstallationRegistry::new();
        impl $installer {
            $installer_visibility fn install() -> Result<
                ::std::sync::Arc<$state>,
                $crate::HookRegistryError,
            > {
                [<__ $installer _registry>].install()?;
                Ok([<$installer:upper _STATE>].read()
                    .as_ref().cloned()
                    .expect("raw hook state is not installed"))
            }
            $installer_visibility fn uninstall() -> Result<(), $crate::HookRegistryError> {
                [<__ $installer _registry>].uninstall()
            }
            $installer_visibility fn with_state<T>(
                select: impl FnOnce(&$state) -> T,
            ) -> Option<T> {
                [<$installer:upper _STATE>].read()
                    .as_deref()
                    .map(select)
            }
        }
        $crate::chain_hooks! {
            #![order($order)]
            #![detour($detour)]
            #![state(::std::sync::Arc<$state> =
                [<$installer:upper _STATE>].read()
                    .as_ref().cloned()
                    .expect("raw hook state is not installed")
            )]
            $($items)*
        }
    }};

    (
        #![order($order:ident)]
        #![detour($detour:ty)]
        #[installer]
        $installer_visibility:vis struct $installer:ident;
        $($items:tt)*
    ) => { $crate::__paste! {
        $crate::chain_hooks!(@declare_installer
            [$installer_visibility] [$installer] [stateless] [$($items)*]
        );
        impl $crate::ManagedHookInstallation for $installer {
            fn create() -> Result<Self, $crate::HookInstallError<Self>> { Self::create() }
            fn uninstall_terminals(&mut self) -> Result<(), $crate::TerminalUninstallError> {
                self.uninstall_terminals()
            }
            fn is_complete(&self) -> bool { self.is_complete() }
            fn is_empty(&self) -> bool { self.is_empty() }
        }
        #[allow(non_upper_case_globals)]
        static [<__ $installer _registry>]:
            $crate::HookInstallationRegistry<$installer> =
                $crate::HookInstallationRegistry::new();
        impl $installer {
            $installer_visibility fn install() -> Result<(), $crate::HookRegistryError> {
                [<__ $installer _registry>].install()
            }
            $installer_visibility fn uninstall() -> Result<(), $crate::HookRegistryError> {
                [<__ $installer _registry>].uninstall()
            }
        }
        $crate::chain_hooks! {
            #![order($order)]
            #![detour($detour)]
            $($items)*
        }
    }};

    (@declare_installer
        [$visibility:vis] [$installer:ident] [stateless] [$($items:tt)*]
    ) => {
        $visibility struct $installer {
            terminals: ::std::vec::Vec<
                ::std::boxed::Box<dyn $crate::ErasedTerminalInstallation>
            >,
            complete: bool,
        }

        impl $installer {
            $visibility fn is_empty(&self) -> bool { self.terminals.is_empty() }
            $visibility fn is_complete(&self) -> bool { self.complete }

            fn create() -> Result<Self, $crate::HookInstallError<Self>> {
                let mut installation = Self {
                    terminals: ::std::vec::Vec::new(),
                    complete: false,
                };
                $crate::chain_hooks!(@install_terminals installation; $($items)*);
                installation.complete = true;
                Ok(installation)
            }

            fn uninstall_terminals(&mut self) -> Result<(), $crate::TerminalUninstallError> {
                let mut first_error = None;
                let mut index = self.terminals.len();
                while index != 0 {
                    index -= 1;
                    match self.terminals[index].uninstall() {
                        Ok(()) => { self.terminals.remove(index); }
                        Err(error) => { first_error.get_or_insert(error); }
                    }
                }
                first_error.map_or(Ok(()), Err)
            }
        }

        impl Drop for $installer {
            fn drop(&mut self) { let _ = self.uninstall_terminals(); }
        }
    };

    (@declare_installer
        [$visibility:vis] [$installer:ident] [state $state:ty] [$($items:tt)*]
    ) => { $crate::__paste! {
        static [<$installer:upper _STATE>]: $crate::__RwLock<
            Option<::std::sync::Arc<$state>>
        > = $crate::__RwLock::new(None);

        $visibility struct $installer {
            terminals: ::std::vec::Vec<
                ::std::boxed::Box<dyn $crate::ErasedTerminalInstallation>
            >,
            _state: ::std::sync::Arc<$state>,
            complete: bool,
        }

        impl $installer {
            $visibility fn is_empty(&self) -> bool { self.terminals.is_empty() }
            $visibility fn is_complete(&self) -> bool { self.complete }

            fn create() -> Result<Self, $crate::HookInstallError<Self>> {
                let state = ::std::sync::Arc::new(<$state as Default>::default());
                *[<$installer:upper _STATE>].write()
                    = Some(state.clone());
                let mut installation = Self {
                    terminals: ::std::vec::Vec::new(),
                    _state: state,
                    complete: false,
                };
                $crate::chain_hooks!(@install_terminals installation; $($items)*);
                installation.complete = true;
                Ok(installation)
            }

            fn uninstall_terminals(&mut self) -> Result<(), $crate::TerminalUninstallError> {
                let mut first_error = None;
                let mut index = self.terminals.len();
                while index != 0 {
                    index -= 1;
                    match self.terminals[index].uninstall() {
                        Ok(()) => { self.terminals.remove(index); }
                        Err(error) => { first_error.get_or_insert(error); }
                    }
                }
                if self.terminals.is_empty() {
                    *[<$installer:upper _STATE>].write()
                        = None;
                }
                first_error.map_or(Ok(()), Err)
            }
        }

        impl Drop for $installer {
            fn drop(&mut self) { let _ = self.uninstall_terminals(); }
        }
    }};

    (@install_terminals $installation:ident;) => {};
    (@install_terminals $installation:ident;
        #[terminal(installed($($terminal:tt)*), fallback = $fallback:expr $(,)?)]
        $(#[prefix($($prefix:tt)*)])?
        $(#[infix($($infix:path),* $(,)? )])?
        $(#[postfix($($postfix:tt)*)])?
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),* $(,)?
        ) -> $result:ty;
        $($remaining:tt)*
    ) => { $crate::__paste! {
        match [<install_ $name _terminal>]() {
            Ok(terminal) => $installation.terminals.push(::std::boxed::Box::new(terminal)),
            Err(source) => {
                let _ = $installation.uninstall_terminals();
                return Err($crate::HookInstallError {
                    source,
                    installation: (!$installation.terminals.is_empty()).then_some($installation),
                });
            }
        }
        $crate::chain_hooks!(@install_terminals $installation; $($remaining)*);
    }};
    (@install_terminals $installation:ident;
        #[terminal(installed($($terminal:tt)*), fallback = $fallback:expr $(,)?)]
        $(#[prefix($($prefix:tt)*)])?
        $(#[infix($($infix:path),* $(,)? )])?
        $(#[postfix($($postfix:tt)*)])?
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),* $(,)?
        );
        $($remaining:tt)*
    ) => { $crate::__paste! {
        match [<install_ $name _terminal>]() {
            Ok(terminal) => $installation.terminals.push(::std::boxed::Box::new(terminal)),
            Err(source) => {
                let _ = $installation.uninstall_terminals();
                return Err($crate::HookInstallError {
                    source,
                    installation: (!$installation.terminals.is_empty()).then_some($installation),
                });
            }
        }
        $crate::chain_hooks!(@install_terminals $installation; $($remaining)*);
    }};
    (@install_terminals $installation:ident; $token:tt $($remaining:tt)*) => {
        $crate::chain_hooks!(@install_terminals $installation; $($remaining)*);
    };

    (
        #![terminal_resolver($resolve:ident)]
        #![order($order:ident)]
        #![detour($detour:ty)]
        #[context]
        $context_visibility:vis struct $common_context:ident {
            $($field_visibility:vis $field:ident: $field_type:ty = $field_value:expr),* $(,)?
        }
        $($items:tt)*
    ) => {
        $context_visibility struct $common_context {
            $($field_visibility $field: $field_type,)*
        }
        $crate::chain_hooks!(@items $order, $resolve, [$detour],
            [common: $common_context = $common_context { $($field: $field_value,)* }];
            $($items)*
        );
    };

    (
        #![order($order:ident)]
        #![detour($detour:ty)]
        #[context]
        $context_visibility:vis struct $common_context:ident {
            $($field_visibility:vis $field:ident: $field_type:ty = $field_value:expr),* $(,)?
        }
        $($items:tt)*
    ) => {
        $context_visibility struct $common_context {
            $($field_visibility $field: $field_type,)*
        }
        $crate::chain_hooks!(@items $order, __unused_terminal_resolver, [$detour],
            [common: $common_context = $common_context { $($field: $field_value,)* }];
            $($items)*
        );
    };

    (
        #![terminal_resolver($resolve:ident)]
        #![order($order:ident)]
        #![detour($detour:ty)]
        $($items:tt)*
    ) => {
        $crate::chain_hooks!(@items $order, $resolve, [$detour], []; $($items)*);
    };

    (
        #![order($order:ident)]
        #![detour($detour:ty)]
        $($items:tt)*
    ) => {
        $crate::chain_hooks!(@items $order, __unused_terminal_resolver, [$detour], []; $($items)*);
    };

    (
        #![terminal_resolver($resolve:ident)]
        #![order($order:ident)]
        #[context]
        $context_visibility:vis struct $common_context:ident {
            $($field_visibility:vis $field:ident: $field_type:ty = $field_value:expr),* $(,)?
        }
        $($items:tt)*
    ) => {
        $context_visibility struct $common_context {
            $($field_visibility $field: $field_type,)*
        }
        $crate::chain_hooks!(@items $order, $resolve, [],
            [common: $common_context = $common_context { $($field: $field_value,)* }];
            $($items)*
        );
    };

    (
        #![order($order:ident)]
        #[context]
        $context_visibility:vis struct $common_context:ident {
            $($field_visibility:vis $field:ident: $field_type:ty = $field_value:expr),* $(,)?
        }
        $($items:tt)*
    ) => {
        $context_visibility struct $common_context {
            $($field_visibility $field: $field_type,)*
        }
        $crate::chain_hooks!(@items $order, __unused_terminal_resolver, [],
            [common: $common_context = $common_context { $($field: $field_value,)* }];
            $($items)*
        );
    };

    (
        #![terminal_resolver($resolve:ident)]
        #![order($order:ident)]
        $($items:tt)*
    ) => {
        $crate::chain_hooks!(@items $order, $resolve, [], []; $($items)*);
    };

    (
        #![order($order:ident)]
        $($items:tt)*
    ) => {
        $crate::chain_hooks!(@items $order, __unused_terminal_resolver, [], []; $($items)*);
    };

    (@items $order:ident, $resolve:ident, $detour:tt, $common:tt;) => {};

    (@items $order:ident, $resolve:ident, $detour:tt, $common:tt;
        #[trampoline]
        $($item:tt)*
    ) => {
        $crate::chain_hooks!(@items $order, $resolve, $detour, $common; $($item)*);
    };

    (@items $order:ident, $resolve:ident, $detour:tt, $common:tt;
        #[trampoline(abi = $trampoline_abi:literal)]
        #[terminal($($terminal:tt)*)]
        $(#[prefix($($prefix:tt)*)])?
        $(#[infix($($infix:path),* $(,)? )])?
        $(#[postfix($($postfix:tt)*)])?
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),* $(,)?
        ) $(-> $result:ty)?;
        $($remaining:tt)*
    ) => {
        $crate::chain_hooks!(@items $order, $resolve, $detour, $common;
            #[terminal($($terminal)*)]
            $(#[prefix($($prefix)*)])?
            $(#[infix($($infix),*)])?
            $(#[postfix($($postfix)*)])?
            $visibility unsafe extern $trampoline_abi fn $name(
                $($argument: $argument_type),*
            ) $(-> $result)?;
            $($remaining)*
        );
    };

    (@items $order:ident, $resolve:ident, $detour:tt, $common:tt;
        #[trampoline(abi = $trampoline_abi:literal)]
        #[terminal($($terminal:tt)*)]
        $(#[prefix($($prefix:tt)*)])?
        $(#[infix($($infix:path),* $(,)? )])?
        $(#[postfix($($postfix:tt)*)])?
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),* $(,)?
        ) $(-> $result:ty)? { $($body:tt)* }
        $($remaining:tt)*
    ) => {
        $crate::chain_hooks!(@items $order, $resolve, $detour, $common;
            #[terminal($($terminal)*)]
            $(#[prefix($($prefix)*)])?
            $(#[infix($($infix),*)])?
            $(#[postfix($($postfix)*)])?
            $visibility unsafe extern $trampoline_abi fn $name(
                $($argument: $argument_type),*
            ) $(-> $result)? { $($body)* }
            $($remaining)*
        );
    };

    (@items $order:ident, $resolve:ident, $detour:tt, $common:tt;
        #[export($target:ident)]
        #[terminal($($terminal:tt)*)]
        $(#[prefix($($prefix:tt)*)])?
        $(#[infix($($infix:path),* $(,)? )])?
        $(#[postfix($($postfix:tt)*)])?
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),* $(,)?
        ) $(-> $result:ty)?;
        $($remaining:tt)*
    ) => {
        $crate::chain_hooks!(@items $order, $resolve, $detour, $common;
            #[terminal($($terminal)*)]
            $(#[prefix($($prefix)*)])?
            $(#[infix($($infix),*)])?
            $(#[postfix($($postfix)*)])?
            $visibility unsafe extern $abi fn $name(
                $($argument: $argument_type),*
            ) $(-> $result)?;
        );
        #[unsafe(no_mangle)]
        pub unsafe extern $abi fn $target(
            $($argument: $argument_type),*
        ) $(-> $result)? {
            unsafe { $name($($argument),*) }
        }
        $crate::chain_hooks!(@items $order, $resolve, $detour, $common; $($remaining)*);
    };

    (@items $order:ident, $resolve:ident, $detour:tt, $common:tt;
        #[export($target:ident)]
        #[terminal($($terminal:tt)*)]
        $(#[prefix($($prefix:tt)*)])?
        $(#[infix($($infix:path),* $(,)? )])?
        $(#[postfix($($postfix:tt)*)])?
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),* $(,)?
        ) $(-> $result:ty)? { $($body:tt)* }
        $($remaining:tt)*
    ) => {
        $crate::chain_hooks!(@items $order, $resolve, $detour, $common;
            #[terminal($($terminal)*)]
            $(#[prefix($($prefix)*)])?
            $(#[infix($($infix),*)])?
            $(#[postfix($($postfix)*)])?
            $visibility unsafe extern $abi fn $name(
                $($argument: $argument_type),*
            ) $(-> $result)? { $($body)* }
        );
        #[unsafe(no_mangle)]
        pub unsafe extern $abi fn $target(
            $($argument: $argument_type),*
        ) $(-> $result)? {
            unsafe { $name($($argument),*) }
        }
        $crate::chain_hooks!(@items $order, $resolve, $detour, $common; $($remaining)*);
    };

    // These two entry arms stay separate because a `ty` fragment cannot be
    // followed by an arbitrary token-tree fragment in `macro_rules!`.
    (@items $order:ident, $resolve:ident, $detour:tt, $common:tt;
        #[terminal($($terminal:tt)*)]
        $(#[prefix($($prefix:tt)*)])?
        $(#[infix($($infix:path),* $(,)? )])?
        $(#[postfix($($postfix:tt)*)])?
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),* $(,)?
        ) $(-> $result:ty)?;
        $($remaining:tt)*
    ) => {
        $crate::chain_hooks!(@normalize_terminal $order, $resolve, $abi, $detour, $common;
            [$($terminal)*]
            [$(prefix($($prefix)*))?]
            [$(infix($($infix),*))?]
            [$(postfix($($postfix)*))?]
            $visibility unsafe extern $abi fn $name(
                $($argument: $argument_type),*
            ) $(-> $result)? { no context; }
            then { $($remaining)* }
        );
    };

    (@items $order:ident, $resolve:ident, $detour:tt, $common:tt;
        #[terminal($($terminal:tt)*)]
        $(#[prefix($($prefix:tt)*)])?
        $(#[infix($($infix:path),* $(,)? )])?
        $(#[postfix($($postfix:tt)*)])?
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),* $(,)?
        ) $(-> $result:ty)? { $($body:tt)* }
        $($remaining:tt)*
    ) => {
        $crate::chain_hooks!(@normalize_terminal $order, $resolve, $abi, $detour, $common;
            [$($terminal)*]
            [$(prefix($($prefix)*))?]
            [$(infix($($infix),*))?]
            [$(postfix($($postfix)*))?]
            $visibility unsafe extern $abi fn $name(
                $($argument: $argument_type),*
            ) $(-> $result)? { $($body)* }
            then { $($remaining)* }
        );
    };

    (@normalize_terminal $order:ident, $resolve:ident, $terminal_abi:literal, $chain_detour:tt, $common:tt;
        [direct = $terminal:path]
        $($item:tt)*
    ) => {
        $crate::chain_hooks!(@finish_item $order, $resolve, $chain_detour, $common;
            [direct terminal $terminal;]
            $($item)*
        );
    };

    (@normalize_terminal $order:ident, $resolve:ident, $terminal_abi:literal, $chain_detour:tt, $common:tt;
        [installed(detour = $detour:ty, target = $target:expr,
            continuation = $continuation:path $(,)?), fallback = $fallback:expr $(,)?]
        $($item:tt)*
    ) => {
        $crate::chain_hooks!(@finish_item $order, $resolve, $chain_detour, $common;
            [owned detour $detour, target $target, continuation $continuation,
                abi $terminal_abi with fallback $fallback;]
            $($item)*
        );
    };

    (@normalize_terminal $order:ident, $resolve:ident, $terminal_abi:tt, [$detour:ty], $common:tt;
        [installed(target = $target:expr,
            continuation = $continuation:path $(,)?), fallback = $fallback:expr $(,)?]
        $($item:tt)*
    ) => {
        $crate::chain_hooks!(@finish_item $order, $resolve, [$detour], $common;
            [owned detour $detour, target $target, continuation $continuation,
                abi $terminal_abi with fallback $fallback;]
            $($item)*
        );
    };

    (@normalize_terminal $order:ident, $resolve:ident, $terminal_abi:tt, [$detour:ty], $common:tt;
        [installed(target = $target:expr $(,)?), fallback = $fallback:expr $(,)?]
        $($item:tt)*
    ) => {
        $crate::chain_hooks!(@finish_item $order, $resolve, [$detour], $common;
            [owned detour $detour, target $target,
                abi $terminal_abi with fallback $fallback;]
            $($item)*
        );
    };

    (@normalize_terminal $order:ident, $resolve:ident, $terminal_abi:literal, $chain_detour:tt, $common:tt;
        [installed(detour = $detour:ty, target = $target:expr $(,)?),
            fallback = $fallback:expr $(,)?]
        $($item:tt)*
    ) => {
        $crate::chain_hooks!(@finish_item $order, $resolve, $chain_detour, $common;
            [owned detour $detour, target $target,
                abi $terminal_abi with fallback $fallback;]
            $($item)*
        );
    };

    (@normalize_terminal $order:ident, $resolve:ident, $terminal_abi:literal, $chain_detour:tt, $common:tt;
        [installed, fallback = $fallback:expr $(,)?]
        $($item:tt)*
    ) => {
        $crate::chain_hooks!(@finish_item $order, $resolve, $chain_detour, $common;
            [installed abi $terminal_abi with fallback $fallback;]
            $($item)*
        );
    };

    (@normalize_terminal $order:ident, $resolve:ident, $terminal_abi:literal, $chain_detour:tt, $common:tt;
        [dynamic($resolver_argument:ident, $symbol:literal: $module:ident::$function_type:ident),
            fallback = $fallback:expr $(,)?]
        $($item:tt)*
    ) => {
        $crate::chain_hooks!(@finish_item $order, $resolve, $chain_detour, $common;
            [dynamic proc $resolver_argument, $symbol: $module::$function_type;
                fallback $fallback;]
            $($item)*
        );
    };

    (@finish_item $order:ident, $resolve:ident, $detour:tt, $common:tt;
        [$($resolve_tokens:tt)*]
        [$($prefix:tt)*]
        [$($infix:tt)*]
        [$($postfix:tt)*]
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),*
        ) $(-> $result:ty)? { $($body:tt)* }
        then { $($remaining:tt)* }
    ) => {
        $crate::chain_hooks! {
            @emit $order, $resolve, $common;
            [$($resolve_tokens)*]
            [$($prefix)*]
            [$($infix)*]
            [$($postfix)*]
            $visibility unsafe extern $abi fn $name(
                $($argument: $argument_type),*
            ) $(-> $result)? { $($body)* }
        }
        $crate::chain_hooks!(@items $order, $resolve, $detour, $common; $($remaining)*);
    };

    (@emit $order:ident, $resolve:ident, [];
        [$($resolve_tokens:tt)*]
        [$($prefix:tt)*]
        [$($infix:tt)*]
        [$($postfix:tt)*]
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),*
        ) -> $result:ty
        { no context; }
    ) => {
        $crate::chain_hooks!(@generate $order, $resolve;
            [$($resolve_tokens)*] [$($prefix)*] [$($infix)*] [$($postfix)*]
            $visibility unsafe extern $abi fn $name($($argument: $argument_type),*) -> $result;
            callback_context no;
            fields {}
        );
    };

    (@emit $order:ident, $resolve:ident, $common:tt;
        [$($resolve_tokens:tt)*] [$($prefix:tt)*] [$($infix:tt)*] [$($postfix:tt)*]
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),*
        ) -> $result:ty { }
    ) => {
        $crate::chain_hooks!(@emit $order, $resolve, $common;
            [$($resolve_tokens)*] [$($prefix)*] [$($infix)*] [$($postfix)*]
            $visibility unsafe extern $abi fn $name($($argument: $argument_type),*) -> $result
            { no context; }
        );
    };

    (@emit $order:ident, $resolve:ident,
        [$shared:ident: $common_type:ty = $common_value:expr];
        [$($resolve_tokens:tt)*] [$($prefix:tt)*] [$($infix:tt)*] [$($postfix:tt)*]
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),*
        ) -> $result:ty { no context; }
    ) => {
        $crate::chain_hooks!(@generate $order, $resolve;
            [$($resolve_tokens)*] [$($prefix)*] [$($infix)*] [$($postfix)*]
            $visibility unsafe extern $abi fn $name($($argument: $argument_type),*) -> $result;
            callback_context yes;
            fields { $shared: $common_type = $common_value }
        );
    };

    (@emit $order:ident, $resolve:ident, [];
        [$($resolve_tokens:tt)*]
        [$($prefix:tt)*]
        [$($infix:tt)*]
        [$($postfix:tt)*]
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),*
        ) -> $result:ty
        { #[context] struct { $($field:ident: $field_type:ty = $field_value:expr),* $(,)? } }
    ) => {
        $crate::chain_hooks!(@generate $order, $resolve;
            [$($resolve_tokens)*] [$($prefix)*] [$($infix)*] [$($postfix)*]
            $visibility unsafe extern $abi fn $name($($argument: $argument_type),*) -> $result;
            callback_context yes;
            fields { $($field: $field_type = $field_value),* }
        );
    };

    (@emit $order:ident, $resolve:ident,
        [$shared:ident: $common_type:ty = $common_value:expr];
        [$($resolve_tokens:tt)*] [$($prefix:tt)*] [$($infix:tt)*] [$($postfix:tt)*]
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),*
        ) -> $result:ty
        { #[context] struct { $($field:ident: $field_type:ty = $field_value:expr),* $(,)? } }
    ) => {
        $crate::chain_hooks!(@generate $order, $resolve;
            [$($resolve_tokens)*] [$($prefix)*] [$($infix)*] [$($postfix)*]
            $visibility unsafe extern $abi fn $name($($argument: $argument_type),*) -> $result;
            callback_context yes;
            fields {
                $shared: $common_type = $common_value,
                $($field: $field_type = $field_value),*
            }
        );
    };

    (@emit $order:ident, $resolve:ident, $common:tt;
        [$($resolve_tokens:tt)*]
        [$($prefix:tt)*]
        [$($infix:tt)*]
        [$($postfix:tt)*]
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),*
        )
        $body:tt
    ) => {
        $crate::chain_hooks!(@emit $order, $resolve, $common;
            [$($resolve_tokens)*] [$($prefix)*] [$($infix)*] [$($postfix)*]
            $visibility unsafe extern $abi fn $name($($argument: $argument_type),*) -> ()
            $body
        );
    };

    (@generate $order:ident, $resolve:ident;
        [$($resolve_tokens:tt)*]
        [$($prefix:tt)*]
        [$($infix:tt)*]
        [$($postfix:tt)*]
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),*
        ) -> $result:ty;
        callback_context $callback_context:ident;
        fields { $($field:ident: $field_type:ty = $field_value:expr),* }
    ) => { $crate::__paste! {
        $crate::chain_hooks!(@declare_terminal
            [$($resolve_tokens)*]
            [$visibility]
            [[<$name:camel Terminal>]]
            [[<$name:camel TerminalFn>]]
            [[<$name:upper _TERMINAL>]]
            [[<install_ $name _terminal>]]
            [$name]
            [$($argument: $argument_type),*]
            [$result]
        );

        $visibility type [<$name:camel Hook>] = unsafe fn(
            $($argument_type,)*
            [<$name:camel Context>]<'_>,
        ) -> $result;

        $visibility struct [<$name:camel Context>]<'a> {
            hooks: &'a [[<$name:camel Hook>]],
            cursor: usize,
            reverse: bool,
            terminal: &'a [<$name:camel Hook>],
            $(#[allow(dead_code)] $visibility $field: &'a $field_type,)*
        }

        impl<'a> $crate::HookChain<'a, [<$name:camel Hook>]>
            for [<$name:camel Context>]<'a>
        {
            fn fp_next(&mut self) -> Option<&'a [<$name:camel Hook>]> {
                if self.reverse {
                    if self.cursor == 0 {
                        Some(self.terminal)
                    } else {
                        self.cursor -= 1;
                        Some(&self.hooks[self.cursor])
                    }
                } else if let Some(hook) = self.hooks.get(self.cursor) {
                    self.cursor += 1;
                    Some(hook)
                } else {
                    Some(self.terminal)
                }
            }
        }

        #[cfg(test)]
        impl<'a> [<$name:camel Context>]<'a> {
            #[allow(dead_code)]
            $visibility fn from_fields($($field: &'a $field_type),*) -> Self {
                #[allow(unused_variables)]
                unsafe fn unavailable(
                    $($argument: $argument_type,)*
                    _context: [<$name:camel Context>]<'_>,
                ) -> $result {
                    panic!("test hook context has no continuation")
                }
                const EMPTY: &[[<$name:camel Hook>]] = &[];
                let terminal: &'static [<$name:camel Hook>] = &(unavailable as [<$name:camel Hook>]);
                Self {
                    hooks: EMPTY,
                    cursor: 0,
                    reverse: false,
                    terminal,
                    $($field,)*
                }
            }
        }

        unsafe fn [<__chain_dispatch_ $name>](
            $($argument: $argument_type),*
        ) -> $result {
            unsafe fn chain_terminal(
                $($argument: $argument_type,)*
                _context: [<$name:camel Context>]<'_>,
            ) -> $result {
                $crate::chain_hooks!(@call_terminal
                    [$resolve]
                    [$($resolve_tokens)*]
                    [[<$name:upper _TERMINAL>]]
                    [$($argument),*]
                )
            }

            $(let $field = $field_value;)*
            let terminal: [<$name:camel Hook>] = chain_terminal;
            let hooks: &[[<$name:camel Hook>]] =
                $crate::chain_hooks!(@infix_slice $($infix)*);
            let reverse = $crate::chain_hooks!(@is_reverse $order);
            let cursor = if reverse { hooks.len() } else { 0 };
            let context = [<$name:camel Context>] {
                hooks,
                cursor,
                reverse,
                terminal: &terminal,
                $($field: &$field,)*
            };

            let prefix_hooks: &[fn(
                $($argument_type,)*
                &[<$name:camel Context>]<'_>,
            )] = $crate::chain_hooks!(@prefix_slice
                [$(($argument, $argument_type))*]
                [[<$name:camel Context>]]
                [$callback_context]
                [];
                $($prefix)*
            );
            if reverse {
                for hook in prefix_hooks.iter().rev() {
                    hook($($argument,)* &context);
                }
            } else {
                for hook in prefix_hooks {
                    hook($($argument,)* &context);
                }
            }

            let mut chain = context;
            let continuation = *$crate::HookChain::fp_next(&mut chain)
                .expect("hook chain has no continuation");
            let result = unsafe { continuation($($argument,)* chain) };

            let context = [<$name:camel Context>] {
                hooks: &[],
                cursor: 0,
                reverse: false,
                terminal: &terminal,
                $($field: &$field,)*
            };
            let postfix_hooks: &[fn(
                $($argument_type,)*
                $result,
                &[<$name:camel Context>]<'_>,
            )] = $crate::chain_hooks!(@postfix_slice
                [$(($argument, $argument_type))*]
                [$result]
                [[<$name:camel Context>]]
                [$callback_context]
                [];
                $($postfix)*
            );
            if reverse {
                for hook in postfix_hooks.iter().rev() {
                    hook($($argument,)* result, &context);
                }
            } else {
                for hook in postfix_hooks {
                    hook($($argument,)* result, &context);
                }
            }
            result
        }

        $visibility unsafe extern $abi fn $name(
            $($argument: $argument_type),*
        ) -> $result {
            unsafe { [<__chain_dispatch_ $name>]($($argument),*) }
        }
    }};

    (@declare_terminal
        [installed abi $terminal_abi:literal with fallback $fallback:expr;]
        [$visibility:vis]
        [$terminal_trait:ident]
        [$terminal_fn:ident]
        [$slot:ident]
        [$install:ident]
        [$hook:ident]
        [$($argument:ident: $argument_type:ty),*]
        [$result:ty]
    ) => {
        $visibility type $terminal_fn = unsafe extern $terminal_abi fn(
            $($argument_type),*
        ) -> $result;

        $visibility trait $terminal_trait: $crate::TerminalLifecycle {
            unsafe fn call(&self, $($argument: $argument_type),*) -> $result;
        }

        impl<T> $terminal_trait for T
        where
            T: $crate::TerminalCallable<Function = $terminal_fn>,
        {
            unsafe fn call(&self, $($argument: $argument_type),*) -> $result {
                let terminal = $crate::TerminalCallable::terminal_callable(self);
                unsafe { terminal($($argument),*) }
            }
        }

        static $slot: $crate::TerminalSlot<dyn $terminal_trait> =
            $crate::TerminalSlot::new();

        $visibility fn $install<T>(
            terminal: T,
        ) -> Result<
            $crate::TerminalInstallation<dyn $terminal_trait>,
            $crate::TerminalInstallError,
        >
        where
            T: $terminal_trait,
        {
            let terminal: ::std::sync::Arc<dyn $terminal_trait> =
                ::std::sync::Arc::new(terminal);
            $slot.install(terminal)
        }
    };
    (@declare_terminal
        [owned detour $detour:ty, target $target:expr,
            abi $terminal_abi:literal with fallback $fallback:expr;]
        [$visibility:vis]
        [$terminal_trait:ident]
        [$terminal_fn:ident]
        [$slot:ident]
        [$install:ident]
        [$hook:ident]
        [$($argument:ident: $argument_type:ty),*]
        [$result:ty]
    ) => {
        $crate::chain_hooks!(@declare_owned_terminal
            [$detour] [$target] [] [$terminal_abi] [$visibility]
            [$terminal_trait] [$terminal_fn] [$slot] [$install] [$hook]
            [$($argument: $argument_type),*] [$result]
        );
    };
    (@declare_terminal
        [owned detour $detour:ty, target $target:expr, continuation $continuation:path,
            abi $terminal_abi:literal with fallback $fallback:expr;]
        [$visibility:vis]
        [$terminal_trait:ident]
        [$terminal_fn:ident]
        [$slot:ident]
        [$install:ident]
        [$hook:ident]
        [$($argument:ident: $argument_type:ty),*]
        [$result:ty]
    ) => {
        $crate::chain_hooks!(@declare_owned_terminal
            [$detour] [$target] [$continuation] [$terminal_abi] [$visibility]
            [$terminal_trait] [$terminal_fn] [$slot] [$install] [$hook]
            [$($argument: $argument_type),*] [$result]
        );
    };
    (@declare_owned_terminal
        [$detour:ty] [$target:expr] [$($continuation:path)?] [$terminal_abi:literal]
        [$visibility:vis] [$terminal_trait:ident] [$terminal_fn:ident]
        [$slot:ident] [$install:ident] [$hook:ident]
        [$($argument:ident: $argument_type:ty),*] [$result:ty]
    ) => {
        $visibility type $terminal_fn = unsafe extern $terminal_abi fn(
            $($argument_type),*
        ) -> $result;

        $visibility trait $terminal_trait: $crate::TerminalLifecycle {
            unsafe fn call(&self, $($argument: $argument_type),*) -> $result;
        }

        impl<T> $terminal_trait for T
        where
            T: $crate::TerminalCallable<Function = $terminal_fn>,
        {
            unsafe fn call(&self, $($argument: $argument_type),*) -> $result {
                let terminal = $crate::TerminalCallable::terminal_callable(self);
                unsafe { terminal($($argument),*) }
            }
        }

        static $slot: $crate::TerminalSlot<dyn $terminal_trait> =
            $crate::TerminalSlot::new();

        $visibility fn $install() -> Result<
            $crate::TerminalInstallation<dyn $terminal_trait>,
            $crate::TerminalInstallError,
        > {
            let terminal = <$detour as $crate::TerminalDetour<$terminal_fn>>::create(
                $target as $terminal_fn,
                $hook as $terminal_fn,
            );
            $(let terminal = $crate::SelectedTerminal::new(terminal, $continuation);)?
            let terminal: ::std::sync::Arc<dyn $terminal_trait> =
                ::std::sync::Arc::new(terminal);
            $slot.install(terminal)
        }
    };
    (@declare_terminal
        [$($resolve_tokens:tt)*]
        [$visibility:vis]
        [$terminal_trait:ident]
        [$terminal_fn:ident]
        [$slot:ident]
        [$install:ident]
        [$hook:ident]
        [$($argument:ident: $argument_type:ty),*]
        [$result:ty]
    ) => {};

    (@call_terminal
        [$resolve:ident]
        [installed abi $terminal_abi:literal with fallback $fallback:expr;]
        [$slot:ident]
        [$($argument:ident),*]
    ) => {
        match $slot.acquire() {
            Some(terminal) => unsafe { terminal.terminal().call($($argument),*) },
            None => $fallback,
        }
    };
    (@call_terminal
        [$resolve:ident]
        [owned detour $detour:ty, target $target:expr,
            abi $terminal_abi:literal with fallback $fallback:expr;]
        [$slot:ident]
        [$($argument:ident),*]
    ) => {
        match $slot.acquire() {
            Some(terminal) => unsafe { terminal.terminal().call($($argument),*) },
            None => $fallback,
        }
    };
    (@call_terminal
        [$resolve:ident]
        [owned detour $detour:ty, target $target:expr, continuation $continuation:path,
            abi $terminal_abi:literal with fallback $fallback:expr;]
        [$slot:ident]
        [$($argument:ident),*]
    ) => {
        match $slot.acquire() {
            Some(terminal) => unsafe { terminal.terminal().call($($argument),*) },
            None => $fallback,
        }
    };
    (@call_terminal
        [$resolve:ident]
        [direct terminal $terminal:path;]
        [$slot:ident]
        [$($argument:ident),*]
    ) => {
        unsafe { $terminal($($argument),*) }
    };
    (@call_terminal
        [$resolve:ident]
        [$($resolve_tokens:tt)*]
        [$slot:ident]
        [$($argument:ident),*]
    ) => {
        ($resolve! {
            ($($argument),*);
            $($resolve_tokens)*
        })()
    };

    (@infix_slice) => { &[] };
    (@infix_slice infix($($hook:path),* $(,)?)) => { &[$($hook),*] };
    (@infix_slice_as $hook_type:ty;) => { &[] as &[$hook_type] };
    (@infix_slice_as $hook_type:ty; infix($($hook:path),* $(,)?)) => {
        &[$($hook as $hook_type),*]
    };

    (@prefix_slice [$($abi:tt)*] [$context:ident] [$callback_context:ident] [$($adapter:expr,)*];) => {
        &[$($adapter),*]
    };
    (@prefix_slice
        [$(($argument:ident, $argument_type:ty))*]
        [$context:ident]
        [$callback_context:ident]
        [$($adapter:expr,)*];
        prefix($($hooks:tt)*)
    ) => {
        $crate::chain_hooks!(@prefix_adapters
            [$(($argument, $argument_type))*] [$context] [$callback_context] [$($adapter,)*]; $($hooks)*
        )
    };
    (@prefix_adapters
        [$(($argument:ident, $argument_type:ty))*]
        [$context:ident]
        [$callback_context:ident]
        [$($adapter:expr,)*];
    ) => { &[$($adapter),*] };
    (@prefix_adapters
        [$(($argument:ident, $argument_type:ty))*]
        [$context:ident]
        [yes]
        [$($adapter:expr,)*];
        |$projection_context:ident| $observer:expr $(, $($remaining:tt)*)?
    ) => {
        $crate::chain_hooks!(@prefix_adapters
            [$(($argument, $argument_type))*]
            [$context]
            [yes]
            [$($adapter,)*
                |$($argument: $argument_type,)* context: &$context<'_>| {
                    let _ = ($(&$argument,)*);
                    let $projection_context = context;
                    $observer
                },
            ];
            $($($remaining)*)?
        )
    };

    (@prefix_adapters
        [$(($argument:ident, $argument_type:ty))*]
        [$context:ident]
        [$callback_context:ident]
        [$($adapter:expr,)*];
        $($hook:ident)::+($($hook_argument:ident),*) $(, $($remaining:tt)*)?
    ) => {
        $crate::chain_hooks!(@prefix_adapters
            [$(($argument, $argument_type))*]
            [$context]
            [$callback_context]
            [$($adapter,)*
                |$($argument: $argument_type,)* context: &$context<'_>| {
                    let _ = ($(&$argument,)*);
                    $crate::chain_hooks!(@call_prefix [$callback_context] context;
                        [$($hook)::+]; $($hook_argument),*
                    )
                },
            ];
            $($($remaining)*)?
        )
    };

    (@call_prefix [no] $context:expr; [$($hook:ident)::+]; $($argument:ident),*) => {{
        let _ = $context;
        unsafe { $($hook)::+ ($($argument),*) }
    }};
    (@call_prefix [yes] $context:expr; [$($hook:ident)::+]; $($argument:ident),*) => {
        unsafe { $($hook)::+ ($($argument,)* $context) }
    };

    (@postfix_slice [$($abi:tt)*] [$result_type:ty] [$context:ident] [$callback_context:ident] [$($adapter:expr,)*];) => {
        &[$($adapter),*]
    };
    (@postfix_slice
        [$(($argument:ident, $argument_type:ty))*]
        [$result_type:ty]
        [$context:ident]
        [$callback_context:ident]
        [$($adapter:expr,)*];
        postfix($($hooks:tt)*)
    ) => {
        $crate::chain_hooks!(@postfix_adapters
            [$(($argument, $argument_type))*] [$result_type] [$context] [$callback_context] [$($adapter,)*]; $($hooks)*
        )
    };
    (@postfix_adapters
        [$(($argument:ident, $argument_type:ty))*]
        [$result_type:ty]
        [$context:ident]
        [$callback_context:ident]
        [$($adapter:expr,)*];
    ) => { &[$($adapter),*] };
    (@postfix_adapters
        [$(($argument:ident, $argument_type:ty))*]
        [$result_type:ty]
        [$context:ident]
        [yes]
        [$($adapter:expr,)*];
        |$projection_result:ident, $projection_context:ident| $observer:expr
        $(, $($remaining:tt)*)?
    ) => {
        $crate::chain_hooks!(@postfix_adapters
            [$(($argument, $argument_type))*]
            [$result_type]
            [$context]
            [yes]
            [$($adapter,)*
                |$($argument: $argument_type,)* result: $result_type, context: &$context<'_>| {
                    let _ = ($(&$argument,)*);
                    let $projection_result = result;
                    let $projection_context = context;
                    $observer
                },
            ];
            $($($remaining)*)?
        )
    };

    (@postfix_adapters
        [$(($argument:ident, $argument_type:ty))*]
        [$result_type:ty]
        [$context:ident]
        [$callback_context:ident]
        [$($adapter:expr,)*];
        $($hook:ident)::+($($hook_argument:ident),*) $(, $($remaining:tt)*)?
    ) => {
        $crate::chain_hooks!(@postfix_adapters
            [$(($argument, $argument_type))*]
            [$result_type]
            [$context]
            [$callback_context]
            [$($adapter,)*
                |$($argument: $argument_type,)* result: $result_type, context: &$context<'_>| {
                    let _ = ($(&$argument,)*);
                    $crate::chain_hooks!(@call_post_mode [$callback_context]
                        result; context; [$($hook)::+];
                        $($hook_argument),*
                    )
                },
            ];
            $($($remaining)*)?
        )
    };

    (@call_post_mode [no] $result:ident; $context:expr; [$($hook:ident)::+];
        $($argument:ident),*
    ) => {{
        let _ = $context;
        $crate::chain_hooks!(@call_post $result; []; [$($hook)::+]; []; $($argument),*)
    }};
    (@call_post_mode [yes] $result:ident; $context:expr; [$($hook:ident)::+];
        $($argument:ident),*
    ) => {
        $crate::chain_hooks!(@call_post $result; [$context,]; [$($hook)::+]; [];
            $($argument),*
        )
    };

    (@is_reverse forward) => { false };
    (@is_reverse reverse) => { true };

    (@call_post $result:ident; [$($suffix:expr,)*]; [$($hook:ident)::+]; [$($mapped:expr,)*];) => {
        unsafe { $($hook)::+ ($($mapped,)* $($suffix),*) }
    };
    (@call_post $result:ident; [$($suffix:expr,)*]; [$($hook:ident)::+]; [$($mapped:expr,)*];
        result $(, $($remaining:ident),*)?
    ) => {
        $crate::chain_hooks!(@call_post $result; [$($suffix,)*]; [$($hook)::+];
            [$($mapped,)* $result,]; $($($remaining),*)?
        )
    };
    (@call_post $result:ident; [$($suffix:expr,)*]; [$($hook:ident)::+]; [$($mapped:expr,)*];
        $argument:ident $(, $($remaining:ident),*)?
    ) => {
        $crate::chain_hooks!(@call_post $result; [$($suffix,)*]; [$($hook)::+];
            [$($mapped,)* $argument,]; $($($remaining),*)?
        )
    };
}

#[cfg(test)]
mod static_chain_tests {
    use parking_lot::Mutex;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    static EVENTS: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    static FORWARD_EVENTS: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

    unsafe fn terminal(value: usize) -> usize {
        EVENTS.lock().push("terminal");
        value * 2
    }

    unsafe fn forward_terminal(value: usize) -> usize {
        FORWARD_EVENTS.lock().push("terminal");
        value * 2
    }

    unsafe fn prefix_one(value: usize, context: &OperationContext<'_>) {
        assert_eq!(*context.offset, 3);
        assert_eq!(value, 4);
        EVENTS.lock().push("prefix-one");
    }

    unsafe fn prefix_two(context: &OperationContext<'_>) {
        assert_eq!(*context.offset, 3);
        EVENTS.lock().push("prefix-two");
    }

    unsafe fn infix_one(value: usize, mut next: OperationContext<'_>) -> usize {
        EVENTS.lock().push("infix-one-enter");
        let result = crate::next!(next, value);
        EVENTS.lock().push("infix-one-exit");
        result
    }

    unsafe fn infix_two(value: usize, mut next: OperationContext<'_>) -> usize {
        EVENTS.lock().push("infix-two-enter");
        let result = crate::next!(next, value);
        EVENTS.lock().push("infix-two-exit");
        result
    }

    unsafe fn postfix_one(result: usize, context: &OperationContext<'_>) {
        assert_eq!(*context.offset, 3);
        assert_eq!(result, 8);
        EVENTS.lock().push("postfix-one");
    }

    unsafe fn postfix_two(value: usize, result: usize, _context: &OperationContext<'_>) {
        assert_eq!((value, result), (4, 8));
        EVENTS.lock().push("postfix-two");
    }

    unsafe fn forward_infix(value: usize, mut next: ForwardOperationContext<'_>) -> usize {
        FORWARD_EVENTS.lock().push("forward-infix");
        crate::next!(next, value)
    }

    unsafe fn forward_prefix() {
        FORWARD_EVENTS.lock().push("forward-prefix");
    }

    unsafe fn forward_postfix(result: usize) {
        assert_eq!(result, 10);
        FORWARD_EVENTS.lock().push("forward-postfix");
    }

    crate::chain_hooks! {
        #![order(reverse)]

        #[terminal(direct = terminal)]
        #[prefix(prefix_one(value), prefix_two())]
        #[infix(infix_one, infix_two)]
        #[postfix(postfix_one(result), postfix_two(value, result))]
        pub(crate) unsafe extern "system" fn operation(value: usize) -> usize {
            #[context] struct {
                offset: usize = 3,
            }
        }
    }

    crate::chain_hooks! {
        #![order(forward)]

        #[terminal(installed, fallback = usize::MAX)]
        pub(crate) unsafe extern "system" fn installed_operation(value: usize) -> usize;
    }

    static ENABLES: AtomicUsize = AtomicUsize::new(0);
    static DISABLES: AtomicUsize = AtomicUsize::new(0);

    struct InstalledTerminal(InstalledOperationTerminalFn);

    impl crate::TerminalLifecycle for InstalledTerminal {
        fn enable(&self) -> Result<(), i32> {
            ENABLES.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn disable(&self) -> Result<(), i32> {
            DISABLES.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    impl crate::TerminalCallable for InstalledTerminal {
        type Function = InstalledOperationTerminalFn;

        fn terminal_callable(&self) -> Self::Function {
            self.0
        }
    }

    unsafe extern "system" fn times_two(value: usize) -> usize {
        value * 2
    }

    unsafe extern "system" fn times_four(value: usize) -> usize {
        value * 4
    }

    unsafe extern "system" fn times_five(value: usize) -> usize {
        value * 5
    }

    crate::chain_hooks! {
        #![order(forward)]

        #[terminal(direct = forward_terminal)]
        #[prefix(forward_prefix())]
        #[infix(forward_infix)]
        #[postfix(forward_postfix(result))]
        pub(crate) unsafe extern "system" fn forward_operation(value: usize) -> usize;
    }

    unsafe extern "system" fn shared_context_terminal(value: usize) -> usize {
        value
    }

    crate::chain_hooks! {
        #![order(forward)]
        #[context]
        struct SharedRawContext {
            value: usize = 7,
        }

        #[terminal(direct = shared_context_terminal)]
        #[prefix(|next| assert_eq!(next.common.value, 7))]
        unsafe extern "system" fn shared_context_operation(value: usize) -> usize;
    }

    #[test]
    fn static_chain_uses_reverse_order_in_each_phase() {
        EVENTS.lock().clear();
        assert_eq!(unsafe { operation(4) }, 8);
        assert_eq!(
            *EVENTS.lock(),
            [
                "prefix-two",
                "prefix-one",
                "infix-two-enter",
                "infix-one-enter",
                "terminal",
                "infix-one-exit",
                "infix-two-exit",
                "postfix-two",
                "postfix-one",
            ]
        );
    }

    #[test]
    fn static_chain_supports_forward_order_and_an_inferred_empty_context() {
        FORWARD_EVENTS.lock().clear();
        assert_eq!(unsafe { forward_operation(5) }, 10);
        assert_eq!(
            *FORWARD_EVENTS.lock(),
            [
                "forward-prefix",
                "forward-infix",
                "terminal",
                "forward-postfix",
            ]
        );
    }

    #[test]
    fn static_chain_supports_a_shared_context() {
        assert_eq!(unsafe { shared_context_operation(9) }, 9);
    }

    #[test]
    fn installed_terminal_can_be_uninstalled_and_reinstalled() {
        ENABLES.store(0, Ordering::Relaxed);
        DISABLES.store(0, Ordering::Relaxed);

        assert_eq!(unsafe { installed_operation(3) }, usize::MAX);
        let first = install_installed_operation_terminal(InstalledTerminal(times_two)).unwrap();
        assert_eq!(unsafe { installed_operation(3) }, 6);
        first.uninstall().unwrap();
        assert_eq!(unsafe { installed_operation(3) }, usize::MAX);

        let second = install_installed_operation_terminal(InstalledTerminal(times_four)).unwrap();
        assert_eq!(unsafe { installed_operation(3) }, 12);
        second.uninstall().unwrap();

        let dropped = install_installed_operation_terminal(InstalledTerminal(times_five)).unwrap();
        assert_eq!(unsafe { installed_operation(3) }, 15);
        drop(dropped);
        assert_eq!(unsafe { installed_operation(3) }, usize::MAX);
        assert_eq!(ENABLES.load(Ordering::Relaxed), 3);
        assert_eq!(DISABLES.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn uninstall_drains_invocation_leases_before_disabling() {
        trait TestTerminal: crate::TerminalLifecycle {}

        struct Lifecycle(Arc<AtomicUsize>);

        impl crate::TerminalLifecycle for Lifecycle {
            fn enable(&self) -> Result<(), i32> {
                Ok(())
            }

            fn disable(&self) -> Result<(), i32> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }

        impl TestTerminal for Lifecycle {}

        static SLOT: crate::TerminalSlot<dyn TestTerminal> = crate::TerminalSlot::new();

        let disables = Arc::new(AtomicUsize::new(0));
        let terminal: Arc<dyn TestTerminal> = Arc::new(Lifecycle(Arc::clone(&disables)));
        let installation = SLOT.install(terminal).unwrap();
        let invocation = SLOT.acquire().unwrap();
        let uninstall = std::thread::spawn(move || installation.uninstall());

        while SLOT.acquire().is_some() {
            std::thread::yield_now();
        }
        assert_eq!(disables.load(Ordering::SeqCst), 0);
        drop(invocation);
        assert_eq!(uninstall.join().unwrap(), Ok(()));
        assert_eq!(disables.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn failed_enable_releases_the_slot_for_retry() {
        trait TestTerminal: crate::TerminalLifecycle {}

        struct Lifecycle(bool);

        impl crate::TerminalLifecycle for Lifecycle {
            fn enable(&self) -> Result<(), i32> {
                if self.0 { Ok(()) } else { Err(17) }
            }

            fn disable(&self) -> Result<(), i32> {
                Ok(())
            }
        }

        impl TestTerminal for Lifecycle {}

        static SLOT: crate::TerminalSlot<dyn TestTerminal> = crate::TerminalSlot::new();

        let failing: Arc<dyn TestTerminal> = Arc::new(Lifecycle(false));
        assert!(matches!(
            SLOT.install(failing),
            Err(crate::TerminalInstallError::Enable(17))
        ));
        let succeeding: Arc<dyn TestTerminal> = Arc::new(Lifecycle(true));
        let installed = SLOT.install(succeeding).unwrap();
        installed.uninstall().unwrap();
    }

    #[test]
    fn failed_disable_restores_the_generation_and_stale_handles_cannot_remove_a_reinstall() {
        trait TestTerminal: crate::TerminalLifecycle {}

        struct Lifecycle(Arc<AtomicUsize>);

        impl crate::TerminalLifecycle for Lifecycle {
            fn enable(&self) -> Result<(), i32> {
                Ok(())
            }

            fn disable(&self) -> Result<(), i32> {
                if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                    Err(23)
                } else {
                    Ok(())
                }
            }
        }

        impl TestTerminal for Lifecycle {}

        static SLOT: crate::TerminalSlot<dyn TestTerminal> = crate::TerminalSlot::new();

        let attempts = Arc::new(AtomicUsize::new(0));
        let terminal: Arc<dyn TestTerminal> = Arc::new(Lifecycle(Arc::clone(&attempts)));
        let first = SLOT.install(terminal).unwrap();
        assert_eq!(
            first.uninstall(),
            Err(crate::TerminalUninstallError::Disable(23))
        );
        assert!(SLOT.acquire().is_some());
        first.uninstall().unwrap();

        let replacement: Arc<dyn TestTerminal> = Arc::new(Lifecycle(Arc::new(AtomicUsize::new(1))));
        let second = SLOT.install(replacement).unwrap();
        assert_eq!(first.uninstall(), Err(crate::TerminalUninstallError::Stale));
        assert!(SLOT.acquire().is_some());
        second.uninstall().unwrap();
    }
}
