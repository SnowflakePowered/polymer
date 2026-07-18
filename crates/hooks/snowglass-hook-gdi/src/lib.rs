//! Early-process GDI paint redirection hooks.
#![allow(unsafe_code)]

use retour::GenericDetour;
use std::{cell::Cell, sync::OnceLock};
use windows_sys::Win32::{
    Foundation::{E_FAIL, HWND, POINT, RECT},
    Graphics::Gdi::{HDC, PAINTSTRUCT},
};

type BeginPaintFn = unsafe extern "system" fn(HWND, *mut PAINTSTRUCT) -> HDC;
type EndPaintFn = unsafe extern "system" fn(HWND, *const PAINTSTRUCT) -> i32;
type BitBltFn = unsafe extern "system" fn(HDC, i32, i32, i32, i32, HDC, i32, i32, u32) -> i32;

#[link(name = "user32")]
unsafe extern "system" {
    #[link_name = "BeginPaint"]
    fn system_begin_paint(hwnd: HWND, paint: *mut PAINTSTRUCT) -> HDC;
    #[link_name = "EndPaint"]
    fn system_end_paint(hwnd: HWND, paint: *const PAINTSTRUCT) -> i32;
    fn GetClientRect(hwnd: HWND, rect: *mut RECT) -> i32;
    fn GetAncestor(hwnd: HWND, flags: u32) -> HWND;
    fn MapWindowPoints(from: HWND, to: HWND, points: *mut POINT, count: u32) -> i32;
}

const GA_ROOT: u32 = 2;

#[link(name = "gdi32")]
unsafe extern "system" {
    #[link_name = "BitBlt"]
    fn system_bit_blt(
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

static BEGIN_PAINT: OnceLock<GenericDetour<BeginPaintFn>> = OnceLock::new();
static END_PAINT: OnceLock<GenericDetour<EndPaintFn>> = OnceLock::new();
static BIT_BLT: OnceLock<GenericDetour<BitBltFn>> = OnceLock::new();
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

unsafe extern "system" fn begin_paint(hwnd: HWND, paint: *mut PAINTSTRUCT) -> HDC {
    let Some(detour) = BEGIN_PAINT.get() else {
        return std::ptr::null_mut();
    };
    let real = unsafe { detour.call(hwnd, paint) };
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

unsafe extern "system" fn bit_blt(
    destination: HDC,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    source: HDC,
    source_x: i32,
    source_y: i32,
    rop: u32,
) -> i32 {
    let Some(detour) = BIT_BLT.get() else {
        return 0;
    };
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
            detour.call(
                destination,
                x,
                y,
                width,
                height,
                source,
                source_x,
                source_y,
                rop,
            )
        };
    };
    let mut origin = POINT::default();
    if hwnd != root {
        unsafe { MapWindowPoints(hwnd, root, &mut origin, 1) };
    }
    let result = unsafe {
        detour.call(
            replacement as HDC,
            x.saturating_add(origin.x),
            y.saturating_add(origin.y),
            width,
            height,
            source,
            source_x,
            source_y,
            rop,
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

unsafe extern "system" fn end_paint(hwnd: HWND, paint: *const PAINTSTRUCT) -> i32 {
    let Some(detour) = END_PAINT.get() else {
        return 0;
    };
    if paint.is_null() {
        return unsafe { detour.call(hwnd, paint) };
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
        unsafe { detour.call(hwnd, &original) }
    } else {
        unsafe { detour.call(hwnd, paint) }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn SnowglassInstallGdiHook() -> i32 {
    if let Some(hook) = BEGIN_PAINT.get() {
        return if hook.is_enabled()
            && END_PAINT.get().is_some_and(|hook| hook.is_enabled())
            && BIT_BLT.get().is_some_and(|hook| hook.is_enabled())
        {
            0
        } else {
            E_FAIL
        };
    }
    let begin = match unsafe {
        GenericDetour::new(
            system_begin_paint as BeginPaintFn,
            begin_paint as BeginPaintFn,
        )
    } {
        Ok(hook) => hook,
        Err(_) => return E_FAIL,
    };
    let end = match unsafe {
        GenericDetour::new(system_end_paint as EndPaintFn, end_paint as EndPaintFn)
    } {
        Ok(hook) => hook,
        Err(_) => return E_FAIL,
    };
    let blit = match unsafe { GenericDetour::new(system_bit_blt as BitBltFn, bit_blt as BitBltFn) }
    {
        Ok(hook) => hook,
        Err(_) => return E_FAIL,
    };
    if BEGIN_PAINT.set(begin).is_err() || END_PAINT.set(end).is_err() || BIT_BLT.set(blit).is_err()
    {
        return E_FAIL;
    }
    let enabled = unsafe {
        BIT_BLT
            .get()
            .expect("blit hook initialized")
            .enable()
            .and_then(|()| END_PAINT.get().expect("end hook initialized").enable())
            .and_then(|()| BEGIN_PAINT.get().expect("begin hook initialized").enable())
    };
    if enabled.is_ok() { 0 } else { E_FAIL }
}
