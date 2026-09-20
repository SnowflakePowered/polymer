//! Injected AppKit desktop isolation.
#![cfg(target_os = "macos")]

mod automation;
mod input;
mod isolation;
mod keyboard;

#[used]
#[unsafe(link_section = "__DATA,__mod_init_func")]
static INITIALIZE: extern "C" fn() = initialize;
extern "C" fn initialize() {
    isolation::Isolation::initialize();
    input::Input::initialize();
}
