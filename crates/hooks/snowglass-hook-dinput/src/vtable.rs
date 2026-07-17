//! COM vtable continuation state.

pub(crate) struct OriginalMethod<T> {
    pub(crate) original_vtable: *const usize,
    pub(crate) intercepted_vtable: *const usize,
    pub(crate) function: T,
}
