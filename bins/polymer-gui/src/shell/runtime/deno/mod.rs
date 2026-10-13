//! JavaScript and DOM bindings for the Blitz-owned shell document.
//!
//! Blitz is the only DOM. JavaScript sees stable wrapper objects keyed by
//! Blitz node IDs, and every read or write goes straight to the document
//! through synchronous Deno ops. Script only runs while Rust holds no borrow
//! of the document.

mod assets;
mod capture;
mod dom;
mod filesystem;
mod input;
mod media;
mod modules;
mod runtime;
mod scroll;
mod shaders;
mod state;

pub use input::EventResult;
pub(crate) use scroll::{ScrollAlignment, ScrollBehavior, ScrollCommand};

pub(crate) use runtime::Runtime;
