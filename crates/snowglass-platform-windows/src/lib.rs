//! Windows-specific handle, adapter, process, and window utilities.

#![forbid(unsafe_code)]

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct ProcessId(pub u32);
