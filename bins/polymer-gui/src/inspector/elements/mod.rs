//! Chrome DevTools Protocol bridge for Blitz's document and layout tree.
//!
//! Deno's inspector owns the V8 domains. This module places a small WebSocket
//! multiplexer in front of it and answers browser DOM/CSS/Page requests on the
//! shell thread, where the Blitz document is owned.

mod css;
mod geometry;
mod protocol;
mod server;

pub(crate) use protocol::{Dispatch, ProtocolError, Reply};
pub(crate) use server::Bridge;
