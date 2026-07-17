//! Draw-time render-region tracking for D3D11 swapchain backbuffers.

use crate::D3d11CaptureRegion;
use dashmap::DashMap;
use std::{ffi::c_void, sync::LazyLock};
use windows::{
    Win32::Graphics::{
        Direct3D11::{
            D3D11_RASTERIZER_DESC, D3D11_VIEWPORT, ID3D11DeviceContext, ID3D11DeviceContext_Vtbl,
            ID3D11DeviceContext4_Vtbl, ID3D11Resource, ID3D11Texture2D,
        },
        Dxgi::IDXGISwapChain,
    },
    core::{IUnknown, Interface},
};

const SLOT_SIZE: usize = std::mem::size_of::<usize>();
const DRAW_INDEXED_SLOT: usize =
    std::mem::offset_of!(ID3D11DeviceContext_Vtbl, DrawIndexed) / SLOT_SIZE;
const DRAW_SLOT: usize = std::mem::offset_of!(ID3D11DeviceContext_Vtbl, Draw) / SLOT_SIZE;
const CONTEXT_VTABLE_LEN: usize = std::mem::size_of::<ID3D11DeviceContext4_Vtbl>() / SLOT_SIZE;

pub(crate) type DrawIndexedFn = unsafe extern "system" fn(*mut c_void, u32, u32, i32);
pub(crate) type DrawFn = unsafe extern "system" fn(*mut c_void, u32, u32);

struct ContextState {
    patched: Box<[usize]>,
    draw_indexed: DrawIndexedFn,
    draw: DrawFn,
}

#[derive(Clone, Copy)]
struct Candidate {
    region: D3d11CaptureRegion,
    area: u64,
}

static CONTEXTS: LazyLock<DashMap<usize, ContextState>> = LazyLock::new(Default::default);
static CANDIDATES: LazyLock<DashMap<usize, Candidate>> = LazyLock::new(Default::default);

pub unsafe fn install_immediate_context(output: *mut *mut c_void) {
    if std::env::var_os("SNOWGLASS_TRACK_RENDER_TARGET").is_none() || output.is_null() {
        return;
    }
    let context = unsafe { *output };
    if context.is_null() {
        return;
    }
    let current = unsafe { *context.cast::<*const usize>() };
    if current.is_null() {
        return;
    }
    if CONTEXTS
        .get(&(context as usize))
        .is_some_and(|state| state.patched.as_ptr() == current)
    {
        return;
    }

    let mut patched = unsafe { std::slice::from_raw_parts(current, CONTEXT_VTABLE_LEN) }.to_vec();
    let original_draw_indexed =
        unsafe { std::mem::transmute::<usize, DrawIndexedFn>(patched[DRAW_INDEXED_SLOT]) };
    let original_draw = unsafe { std::mem::transmute::<usize, DrawFn>(patched[DRAW_SLOT]) };
    patched[DRAW_INDEXED_SLOT] = draw_indexed as *const () as usize;
    patched[DRAW_SLOT] = draw as *const () as usize;
    let state = ContextState {
        patched: patched.into_boxed_slice(),
        draw_indexed: original_draw_indexed,
        draw: original_draw,
    };
    unsafe { *context.cast::<*const usize>() = state.patched.as_ptr() };
    CONTEXTS.insert(context as usize, state);
}

fn draw_indexed_original(context: *mut c_void) -> Option<DrawIndexedFn> {
    CONTEXTS
        .get(&(context as usize))
        .map(|state| state.draw_indexed)
}

fn draw_original(context: *mut c_void) -> Option<DrawFn> {
    CONTEXTS.get(&(context as usize)).map(|state| state.draw)
}

fn record_draw(raw_context: *mut c_void) {
    let Some(context) = (unsafe { ID3D11DeviceContext::from_raw_borrowed(&raw_context) }) else {
        return;
    };
    let mut targets = [None];
    unsafe { context.OMGetRenderTargets(Some(&mut targets), None) };
    let Some(target) = targets[0].as_ref() else {
        return;
    };
    let Ok(resource) = (unsafe { target.GetResource() }) else {
        return;
    };
    let Some(region) = current_region(context) else {
        return;
    };
    let area = u64::from(region.width) * u64::from(region.height);
    let key = identity_key(&resource);
    CANDIDATES
        .entry(key)
        .and_modify(|candidate| {
            if area > candidate.area {
                *candidate = Candidate { region, area };
            }
        })
        .or_insert(Candidate { region, area });
}

unsafe extern "system" fn draw_indexed(
    this: *mut c_void,
    index_count: u32,
    start_index: u32,
    base_vertex: i32,
) {
    if let Some(original) = draw_indexed_original(this) {
        unsafe { original(this, index_count, start_index, base_vertex) };
    }
    record_draw(this);
}

unsafe extern "system" fn draw(this: *mut c_void, vertex_count: u32, start_vertex: u32) {
    if let Some(original) = draw_original(this) {
        unsafe { original(this, vertex_count, start_vertex) };
    }
    record_draw(this);
}

pub(crate) fn take_swapchain_region(swapchain: *mut c_void) -> Option<D3d11CaptureRegion> {
    std::env::var_os("SNOWGLASS_TRACK_RENDER_TARGET")?;
    let swapchain = unsafe { IDXGISwapChain::from_raw_borrowed(&swapchain) }?;
    let texture: ID3D11Texture2D = unsafe { swapchain.GetBuffer(0).ok()? };
    let resource: ID3D11Resource = texture.cast().ok()?;
    CANDIDATES
        .remove(&identity_key(&resource))
        .map(|(_, value)| value.region)
}

fn current_region(context: &ID3D11DeviceContext) -> Option<D3d11CaptureRegion> {
    let mut viewport_count = 1;
    let mut viewport = D3D11_VIEWPORT::default();
    unsafe { context.RSGetViewports(&mut viewport_count, Some(&mut viewport)) };
    if viewport_count == 0 {
        return None;
    }
    let mut left = viewport.TopLeftX.round() as i64;
    let mut top = viewport.TopLeftY.round() as i64;
    let mut right = (viewport.TopLeftX + viewport.Width).round() as i64;
    let mut bottom = (viewport.TopLeftY + viewport.Height).round() as i64;

    if let Ok(rasterizer) = unsafe { context.RSGetState() } {
        let mut desc = D3D11_RASTERIZER_DESC::default();
        unsafe { rasterizer.GetDesc(&mut desc) };
        if desc.ScissorEnable.as_bool() {
            let mut count = 1;
            let mut scissor = windows::Win32::Foundation::RECT::default();
            unsafe { context.RSGetScissorRects(&mut count, Some(&mut scissor)) };
            if count != 0 {
                left = left.max(i64::from(scissor.left));
                top = top.max(i64::from(scissor.top));
                right = right.min(i64::from(scissor.right));
                bottom = bottom.min(i64::from(scissor.bottom));
            }
        }
    }
    left = left.max(0);
    top = top.max(0);
    (right > left && bottom > top).then_some(D3d11CaptureRegion {
        x: left as u32,
        y: top as u32,
        width: (right - left) as u32,
        height: (bottom - top) as u32,
    })
}

fn identity_key(resource: &ID3D11Resource) -> usize {
    resource
        .cast::<IUnknown>()
        .map_or(resource.as_raw() as usize, |identity| {
            identity.as_raw() as usize
        })
}
