//! Early-process GDI paint redirection hooks.
#![allow(unsafe_code)]

use snowglass_hook::HookChain;
use std::cell::Cell;
use windows_sys::Win32::{
    Foundation::{E_FAIL, HWND, POINT, RECT},
    Graphics::Gdi::{HDC, PAINTSTRUCT},
};

#[link(name = "user32")]
unsafe extern "system" {
    #[link_name = "BeginPaint"]
    pub(crate) fn system_begin_paint(hwnd: HWND, paint: *mut PAINTSTRUCT) -> HDC;
    #[link_name = "EndPaint"]
    pub(crate) fn system_end_paint(hwnd: HWND, paint: *const PAINTSTRUCT) -> i32;
    fn GetClientRect(hwnd: HWND, rect: *mut RECT) -> i32;
    fn GetAncestor(hwnd: HWND, flags: u32) -> HWND;
    fn MapWindowPoints(from: HWND, to: HWND, points: *mut POINT, count: u32) -> i32;
}

const GA_ROOT: u32 = 2;

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
    fn WindowFromDC(hdc: HDC) -> HWND;
}

snowglass_hook::chain_hooks! {
    #![order(reverse)]
    #![detour(snowglass_platform_windows::InlineDetour<_>)]
    #[installer]
    pub(crate) struct HookInstallation;

    #[trampoline]
    #[terminal(installed(target = system_begin_paint), fallback = std::ptr::null_mut())]
    #[infix(redirect_begin_paint)]
    unsafe extern "system" fn begin_paint(hwnd: HWND, paint: *mut PAINTSTRUCT) -> HDC;

    #[trampoline]
    #[terminal(installed(target = system_end_paint), fallback = 0)]
    #[infix(redirect_end_paint)]
    unsafe extern "system" fn end_paint(hwnd: HWND, paint: *const PAINTSTRUCT) -> i32;

    #[trampoline]
    #[terminal(installed(target = system_bit_blt), fallback = 0)]
    #[infix(redirect_bit_blt)]
    unsafe extern "system" fn bit_blt(
        destination: HDC, x: i32, y: i32, width: i32, height: i32,
        source: HDC, source_x: i32, source_y: i32, rop: u32,
    ) -> i32;
}

thread_local! { static INSIDE_HOOK: Cell<bool> = const { Cell::new(false) }; }
thread_local! { static REAL_PAINT_DC: Cell<Option<(isize, isize)>> = const { Cell::new(None) }; }

unsafe fn replacement_dc(hwnd: HWND) -> Option<isize> {
    if hwnd.is_null() {
        return None;
    }
    let mut client = RECT::default();
    if unsafe { GetClientRect(hwnd, &mut client) } == 0 {
        return None;
    }
    unsafe {
        snowglass_producer_gdi::begin_paint(
            hwnd as isize,
            (client.right - client.left).max(0) as u32,
            (client.bottom - client.top).max(0) as u32,
            [client.left, client.top, client.right, client.bottom],
        )
    }
}

unsafe fn redirect_begin_paint(
    hwnd: HWND,
    paint: *mut PAINTSTRUCT,
    mut context: BeginPaintContext<'_>,
) -> HDC {
    let next = *context
        .fp_next()
        .expect("GDI hook chains always have a terminal");
    let real = unsafe { next(hwnd, paint, context) };
    if real.is_null() || paint.is_null() || hwnd.is_null() {
        return real;
    }
    let redirect = INSIDE_HOOK.with(|inside| {
        if inside.replace(true) {
            return None;
        }
        let result = if unsafe { GetAncestor(hwnd, GA_ROOT) } != hwnd {
            None
        } else {
            let mut client = RECT::default();
            if unsafe { GetClientRect(hwnd, &mut client) } == 0 {
                None
            } else {
                let dirty = unsafe { (*paint).rcPaint };
                unsafe {
                    snowglass_producer_gdi::begin_paint(
                        hwnd as isize,
                        (client.right - client.left).max(0) as u32,
                        (client.bottom - client.top).max(0) as u32,
                        [dirty.left, dirty.top, dirty.right, dirty.bottom],
                    )
                }
            }
        };
        inside.set(false);
        result
    });
    if let Some(dc) = redirect {
        REAL_PAINT_DC.with(|saved| saved.set(Some((hwnd as isize, real as isize))));
        unsafe { (*paint).hdc = dc as HDC };
        dc as HDC
    } else {
        real
    }
}

unsafe fn redirect_bit_blt(
    destination: HDC,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    source: HDC,
    source_x: i32,
    source_y: i32,
    rop: u32,
    mut context: BitBltContext<'_>,
) -> i32 {
    let next = *context
        .fp_next()
        .expect("GDI hook chains always have a terminal");
    let hwnd = unsafe { WindowFromDC(destination) };
    let root = if hwnd.is_null() {
        std::ptr::null_mut()
    } else {
        unsafe { GetAncestor(hwnd, GA_ROOT) }
    };
    let replacement = INSIDE_HOOK.with(|inside| {
        if inside.replace(true) {
            return None;
        }
        let result = if root.is_null() {
            None
        } else {
            unsafe { replacement_dc(root) }
        };
        inside.set(false);
        result
    });
    let Some(replacement) = replacement else {
        return unsafe {
            next(
                destination,
                x,
                y,
                width,
                height,
                source,
                source_x,
                source_y,
                rop,
                context,
            )
        };
    };
    let mut origin = POINT::default();
    if hwnd != root {
        unsafe { MapWindowPoints(hwnd, root, &mut origin, 1) };
    }
    let result = unsafe {
        next(
            replacement as HDC,
            x.saturating_add(origin.x),
            y.saturating_add(origin.y),
            width,
            height,
            source,
            source_x,
            source_y,
            rop,
            context,
        )
    };
    INSIDE_HOOK.with(|inside| {
        if !inside.replace(true) {
            unsafe { snowglass_producer_gdi::end_paint(root as isize, replacement) };
            inside.set(false);
        }
    });
    result
}

unsafe fn redirect_end_paint(
    hwnd: HWND,
    paint: *const PAINTSTRUCT,
    mut context: EndPaintContext<'_>,
) -> i32 {
    let next = *context
        .fp_next()
        .expect("GDI hook chains always have a terminal");
    if paint.is_null() {
        return unsafe { next(hwnd, paint, context) };
    }
    let redirected = unsafe { (*paint).hdc };
    INSIDE_HOOK.with(|inside| {
        if !inside.replace(true) {
            unsafe { snowglass_producer_gdi::end_paint(hwnd as isize, redirected as isize) };
            inside.set(false);
        }
    });
    let real = REAL_PAINT_DC.with(|saved| saved.take());
    if let Some((saved_hwnd, real_dc)) = real
        && saved_hwnd == hwnd as isize
    {
        let mut original = unsafe { *paint };
        original.hdc = real_dc as HDC;
        unsafe { next(hwnd, &original, context) }
    } else {
        unsafe { next(hwnd, paint, context) }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn SnowglassInstallGdiHook() -> i32 {
    HookInstallation::install().map_or(E_FAIL, |()| 0)
}
