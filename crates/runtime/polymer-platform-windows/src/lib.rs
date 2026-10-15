//! Windows-specific handle, process, desktop, timing, and window utilities.

mod automation;
mod desktop;
mod detour;
mod handles;
mod isolation;
mod process;
mod timing;
mod types;
mod window;

pub use automation::WindowsAutomationDriver;
pub use desktop::{DesktopThreadAttachment, PrivateDesktop};
pub use detour::{InlineDetour, call_original};
pub use handles::duplicate_handle_into_process;
pub use isolation::KillOnCloseJob;
pub use process::{
    ProcessArchitecture, executable_architecture, process_architecture, process_tree_ids,
    resume_initial_thread,
};
pub use timing::{performance_counter, performance_frequency};
pub use types::{NativeWindow, ProcessId};
pub use window::{
    WindowKeyForwarder, client_size, find_main_window, forward_character, forward_focus,
    forward_key, forward_mouse_button, forward_mouse_move, forward_mouse_wheel, forward_raw_key,
    is_borderless_window, is_process_window, keyboard_code, window_title,
};

mod fixed_window;
pub use fixed_window::FixedWindowBinding;
mod window_lifetime;
pub use window_lifetime::WindowLifetime;

mod guest;
pub use guest::GuestPlatform;

mod graphics;
pub use graphics::GraphicsSupport;

mod output_watcher;
pub use output_watcher::DefaultOutputWatcher;
