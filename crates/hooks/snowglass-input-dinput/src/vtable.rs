//! COM vtable continuation helpers.

use std::ffi::c_void;

pub(crate) struct OriginalMethod<T> {
    pub(crate) original_vtable: *const usize,
    pub(crate) intercepted_vtable: *const usize,
    pub(crate) function: T,
}

/// Calls an original COM method while the object points at its original
/// vtable, then restores the intercepted vtable before returning.
pub(crate) unsafe fn call_original<R>(
    this: *mut c_void,
    method: OriginalMethod<impl Copy>,
    call: impl FnOnce() -> R,
) -> R {
    let slot = this.cast::<*const usize>();
    unsafe { *slot = method.original_vtable };
    let result = call();
    unsafe { *slot = method.intercepted_vtable };
    result
}
