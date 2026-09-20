//! Lifecycle backends for exported entry points and Objective-C methods.
use crate::{TerminalCallable, TerminalDetour, TerminalLifecycle};
use objc2::runtime::{Imp, Method};
use std::{marker::PhantomData, mem};

/// An entry point selected by the guest's dynamic API loader.
/// The export is permanently present; the terminal slot controls dispatch.
pub struct ExportDetour<F>(F);
impl<F: Copy + Send + Sync + 'static> TerminalLifecycle for ExportDetour<F> {
    fn enable(&self) -> Result<(), i32> {
        Ok(())
    }
    fn disable(&self) -> Result<(), i32> {
        Ok(())
    }
}
impl<F: Copy + Send + Sync + 'static> TerminalCallable for ExportDetour<F> {
    type Function = F;
    fn terminal_callable(&self) -> F {
        self.0
    }
}
impl<F: Copy + Send + Sync + 'static> TerminalDetour<F> for ExportDetour<F> {
    fn create(target: F, _detour: F) -> Self {
        Self(target)
    }
}

/// Identifies the method whose ABI matches the hook's function signature.
///
/// # Safety
/// The method must be owned by the intended class (not an inherited method),
/// and its implementation must have the function type `F`.
pub unsafe trait ObjcMethod<F>: Send + Sync + 'static {
    fn method() -> &'static Method;
}

/// Owns an Objective-C implementation replacement and its original callable.
pub struct ObjcSwizzle<F, M> {
    original: F,
    replacement: F,
    marker: PhantomData<M>,
}
impl<F: Copy, M: ObjcMethod<F>> ObjcSwizzle<F, M> {
    pub fn target() -> F {
        let imp = M::method().implementation();
        assert_eq!(mem::size_of::<F>(), mem::size_of::<Imp>());
        unsafe { mem::transmute_copy(&imp) }
    }
}
impl<F: Copy + Send + Sync + 'static, M: ObjcMethod<F>> TerminalLifecycle for ObjcSwizzle<F, M> {
    fn enable(&self) -> Result<(), i32> {
        unsafe {
            M::method().set_implementation(mem::transmute_copy(&self.replacement));
        }
        Ok(())
    }
    fn disable(&self) -> Result<(), i32> {
        let current = M::method().implementation();
        let replacement: Imp = unsafe { mem::transmute_copy(&self.replacement) };
        // Never overwrite a later swizzle installed by another owner.
        if !std::ptr::fn_addr_eq(current, replacement) {
            return Err(-1);
        }
        unsafe {
            M::method().set_implementation(mem::transmute_copy(&self.original));
        }
        Ok(())
    }
}
impl<F: Copy + Send + Sync + 'static, M: ObjcMethod<F>> TerminalCallable for ObjcSwizzle<F, M> {
    type Function = F;
    fn terminal_callable(&self) -> F {
        self.original
    }
}
impl<F: Copy + Send + Sync + 'static, M: ObjcMethod<F>> TerminalDetour<F> for ObjcSwizzle<F, M> {
    fn create(original: F, replacement: F) -> Self {
        Self {
            original,
            replacement,
            marker: PhantomData,
        }
    }
}
