//! Early-process interception of modal Windows user-interface calls.
#![allow(unsafe_code)]

mod hooks;
mod install;
mod qt;
mod runtime;
mod sys;
