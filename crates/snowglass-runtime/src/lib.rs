//! Once-per-process runtime shared by graphics and input hooks.

#![forbid(unsafe_code)]

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeState {
    Uninitialized,
    Connecting,
    Active,
    ShuttingDown,
}
