//! JavaScript and DOM bindings for the Blitz-owned shell document.
//!
//! Blitz remains the authoritative DOM. JavaScript sees stable wrapper objects
//! backed by snapshots of that DOM, and mutations are immediately mirrored in
//! JavaScript and queued back to Blitz through Deno ops. Keeping the bridge
//! command-based avoids borrowing the document from inside V8 callbacks.

mod dom;
mod input;
mod modules;
mod runtime;
mod state;

pub use dom::Mutation;
pub use input::EventResult;
pub use runtime::Runtime;
pub use snowglass_deno::snowglass::InstanceToken;
pub use snowglass_runtime::{
    EmulatorCommand, EmulatorCommandSink, InputCommand, InputCommandSink, WebRtcCommand,
    WebRtcCommandSink,
};
