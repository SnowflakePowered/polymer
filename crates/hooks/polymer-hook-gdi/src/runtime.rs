//! GDI paint redirection callbacks and per-thread state.

use std::cell::Cell;
use windows_sys::Win32::{
    Foundation::{HWND, POINT, RECT},
    Graphics::Gdi::{HDC, PAINTSTRUCT},
};

const GA_ROOT: u32 = 2;

#[link(name = "user32")]
unsafe extern "system" {
    fn GetClientRect(hwnd: HWND, rect: *mut RECT) -> i32;
    fn GetAncestor(hwnd: HWND, flags: u32) -> HWND;
    fn MapWindowPoints(from: HWND, to: HWND, points: *mut POINT, count: u32) -> i32;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn WindowFromDC(hdc: HDC) -> HWND;
}

thread_local! { static INSIDE_HOOK: Cell<bool> = const { Cell::new(false) }; }
thread_local! { static REAL_PAINT_DC: Cell<Option<(isize, isize)>> = const { Cell::new(None) }; }

pub(crate) unsafe fn redirect_destroy_window(
    hwnd: HWND,
    mut context: crate::hooks::DestroyWindowContext<'_>,
) -> i32 {
    let result = polymer_hook::next!(context, hwnd);
    if result == 0 {
        return result;
    }
    INSIDE_HOOK.with(|inside| {
        if inside.replace(true) {
            return;
        }
        polymer_producer_gdi::window_destroyed(hwnd as isize);
        inside.set(false);
    });
    result
}

unsafe fn replacement_dc(hwnd: HWND) -> Option<isize> {
    if hwnd.is_null() {
        return None;
    }
    let mut client = RECT::default();
    if unsafe { GetClientRect(hwnd, &mut client) } == 0 {
        return None;
    }
    unsafe {
        polymer_producer_gdi::begin_paint(
            hwnd as isize,
            (client.right - client.left).max(0) as u32,
            (client.bottom - client.top).max(0) as u32,
            [client.left, client.top, client.right, client.bottom],
        )
    }
}

pub(crate) unsafe fn redirect_begin_paint(
    hwnd: HWND,
    paint: *mut PAINTSTRUCT,
    mut context: crate::hooks::BeginPaintContext<'_>,
) -> HDC {
    let real = polymer_hook::next!(context, hwnd, paint);
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
                    polymer_producer_gdi::begin_paint(
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

#[expect(clippy::too_many_arguments)]
pub(crate) unsafe fn redirect_bit_blt(
    destination: HDC,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    source: HDC,
    source_x: i32,
    source_y: i32,
    rop: u32,
    mut context: crate::hooks::BitBltContext<'_>,
) -> i32 {
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
        return polymer_hook::next!(
            context,
            destination,
            x,
            y,
            width,
            height,
            source,
            source_x,
            source_y,
            rop,
        );
    };
    let mut origin = POINT::default();
    if hwnd != root {
        unsafe { MapWindowPoints(hwnd, root, &mut origin, 1) };
    }
    let result = polymer_hook::next!(
        context,
        replacement as HDC,
        x.saturating_add(origin.x),
        y.saturating_add(origin.y),
        width,
        height,
        source,
        source_x,
        source_y,
        rop,
    );
    INSIDE_HOOK.with(|inside| {
        if !inside.replace(true) {
            unsafe { polymer_producer_gdi::end_paint(root as isize, replacement) };
            inside.set(false);
        }
    });
    result
}

pub(crate) unsafe fn redirect_end_paint(
    hwnd: HWND,
    paint: *const PAINTSTRUCT,
    mut context: crate::hooks::EndPaintContext<'_>,
) -> i32 {
    if paint.is_null() {
        return polymer_hook::next!(context, hwnd, paint);
    }
    let redirected = unsafe { (*paint).hdc };
    INSIDE_HOOK.with(|inside| {
        if !inside.replace(true) {
            unsafe { polymer_producer_gdi::end_paint(hwnd as isize, redirected as isize) };
            inside.set(false);
        }
    });
    let real = REAL_PAINT_DC.with(|saved| saved.take());
    if let Some((saved_hwnd, real_dc)) = real
        && saved_hwnd == hwnd as isize
    {
        let mut original = unsafe { *paint };
        original.hdc = real_dc as HDC;
        polymer_hook::next!(context, hwnd, &original)
    } else {
        polymer_hook::next!(context, hwnd, paint)
    }
}
