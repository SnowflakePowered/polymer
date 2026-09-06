//! Process-wide GDI detour installation and system entry points.

use windows_sys::Win32::{
    Foundation::{E_FAIL, HWND},
    Graphics::Gdi::{HDC, PAINTSTRUCT},
};

#[link(name = "user32")]
unsafe extern "system" {
    #[link_name = "BeginPaint"]
    pub(crate) fn system_begin_paint(hwnd: HWND, paint: *mut PAINTSTRUCT) -> HDC;
    #[link_name = "EndPaint"]
    pub(crate) fn system_end_paint(hwnd: HWND, paint: *const PAINTSTRUCT) -> i32;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    #[link_name = "BitBlt"]
    pub(crate) fn system_bit_blt(
        destination: HDC,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        source: HDC,
        source_x: i32,
        source_y: i32,
        rop: u32,
    ) -> i32;
}

pub(crate) unsafe fn install() -> i32 {
    crate::hooks::HookInstallation::install().map_or(E_FAIL, |()| 0)
}
