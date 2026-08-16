//! JavaScript and DOM bindings for the Blitz-owned shell document.
//!
//! Blitz remains the authoritative DOM. JavaScript sees stable wrapper objects
//! backed by snapshots of that DOM, and mutations are immediately mirrored in
//! JavaScript and queued back to Blitz through Deno ops. Keeping the bridge
//! command-based avoids borrowing the document from inside V8 callbacks.

mod dom;
mod emulator;
mod input;
mod launcher;
mod modules;
mod runtime;
mod state;

pub use dom::Mutation;
pub use emulator::{EmulatorCommand, EmulatorCommandSink, InstanceToken};
pub use input::EventResult;
pub use launcher::{LauncherCommand, LauncherCommandSink};
pub use runtime::Runtime;

#[cfg(test)]
mod tests;
