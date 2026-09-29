//! Shared typed-hook infrastructure.

mod chain;
pub mod config;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(windows)]
pub mod module;
mod registry;
pub mod telemetry;
mod terminal;
mod vtable;

#[doc(hidden)]
pub use dashmap::DashMap as __DashMap;
#[doc(hidden)]
pub use parking_lot::{Mutex as __Mutex, RwLock as __RwLock};
#[doc(hidden)]
pub use paste::paste as __paste;
pub use registry::{
    HookInstallError, HookInstallationRegistry, HookRegistryError, ManagedHookInstallation,
};
pub use terminal::{
    ErasedTerminalInstallation, HookChain, SelectedTerminal, TerminalCall, TerminalCallable,
    TerminalDetour, TerminalInstallError, TerminalInstallation, TerminalLifecycle, TerminalSlot,
    TerminalUninstallError,
};
pub use vtable::ComProxy;
pub use vtable::VtableHook;
#[doc(hidden)]
pub use windows_core::GUID as __GUID;

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
