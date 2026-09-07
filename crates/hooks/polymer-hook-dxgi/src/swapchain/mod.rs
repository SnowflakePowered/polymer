//! Creation-time dispatch and the shared virtual DXGI COM frontend.
mod creation;
pub(crate) use creation::{CreationDispatch, create_for_hwnd, create_legacy};
pub use creation::{SwapchainCreateInfo, VirtualSwapchainClaim};

mod object;
pub use object::{VirtualDxgiDriver, VirtualDxgiSwapchain};
