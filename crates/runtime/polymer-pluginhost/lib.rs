//! Isolated Polymer plugin registry and runtime.
//!
//! Package discovery is kept separate from execution. Every call made through
//! [`runtime::PluginRuntimeHandle`] runs in a fresh curated Deno/V8 isolate owned by this
//! crate, and invocation-scoped resources are destroyed with that isolate.

#![forbid(unsafe_code)]

pub mod actions;
mod automation;
mod cache;
mod configuration;
pub mod controller;
mod database;
mod download;
pub mod digest;
pub mod emulator;
pub mod filesystem;
mod firmware;
mod handle;
mod keys;
pub mod loading;
pub mod platform;
pub mod plugin;
mod plugin_storage;
pub mod runtime;
pub mod scraper;
pub mod theme;
