//! Carries the graphics hooks into the processes WebView2 spawns.
//!
//! WebView2 renders in a GPU process two levels below its host, and only
//! presents a swapchain when a page is hosted in a window; visual hosting
//! composes into DirectComposition and never presents at all. That GPU process
//! is not spawned by the host, so it cannot be hooked at creation the way a
//! launched guest is, and hooking it afterwards is too late because its
//! swapchain already exists. This hook sits on `CreateProcessW` and hands the
//! graphics hooks to each WebView2 child before it runs, including itself, so
//! the chain reaches the GPU process the browser starts in turn.
#![allow(unsafe_code)]

mod hooks;
mod install;
mod runtime;

pub use runtime::{ARTIFACT_DIR, DESKTOP};

#[unsafe(no_mangle)]
pub unsafe extern "system" fn SnowglassInstallWebView2Hook() -> i32 {
    unsafe { install::install() }
}

#[unsafe(no_mangle)]
pub extern "system" fn SnowglassUninstallWebView2Hook() -> i32 {
    install::uninstall()
}
