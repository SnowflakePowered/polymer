use polymer_platform_windows::NativeWindow;

/// Native window metadata collection, separate from computational assignment.
pub struct ProducerWindows;

impl ProducerWindows {
    pub fn client_extent(window: NativeWindow) -> windows::core::Result<crate::Extent2D> {
        use windows::Win32::{
            Foundation::{E_INVALIDARG, HWND, RECT},
            UI::WindowsAndMessaging::GetClientRect,
        };
        let mut rect = RECT::default();
        unsafe {
            GetClientRect(HWND(window.0 as *mut _), &mut rect)?;
        }
        if rect.right <= rect.left || rect.bottom <= rect.top {
            return Err(windows::core::Error::from_hresult(E_INVALIDARG));
        }
        Ok(crate::Extent2D {
            width: (rect.right - rect.left) as u32,
            height: (rect.bottom - rect.top) as u32,
        })
    }

    /// Called after the fixed assignment commits. Keep the returned guard on
    /// the application-facing object until that binding ends, not on an IPC
    /// connection that can disconnect independently.
    pub fn bind_fixed(
        binding: &crate::swapchain::FixedSurfaceBinding,
    ) -> windows::core::Result<polymer_platform_windows::FixedWindowBinding> {
        polymer_platform_windows::FixedWindowBinding::new(NativeWindow(
            binding.window().handle as isize,
        ))
    }
}
