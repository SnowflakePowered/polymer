use dashmap::DashMap;
use std::ffi::c_void;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ptr::NonNull;
use std::sync::LazyLock;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11DeviceContext_Vtbl, ID3D11DeviceContext1_Vtbl, ID3D11DeviceContext2_Vtbl,
    ID3D11DeviceContext3_Vtbl, ID3D11DeviceContext4_Vtbl,
};

snowglass_hook::vtable! {
    ID3D11DeviceContext_Vtbl,
    ID3D11DeviceContext1_Vtbl,
    ID3D11DeviceContext2_Vtbl,
    ID3D11DeviceContext3_Vtbl,
    ID3D11DeviceContext4_Vtbl,
}

const SLOT_SIZE: usize = size_of::<usize>();
const CONTEXT_SLOT_CAPACITY: usize = <ID3D11DeviceContext4_Vtbl as Vtable>::SLOTS;

type DrawIndexedFn = unsafe extern "system" fn(*mut c_void, u32, u32, i32);
type DrawFn = unsafe extern "system" fn(*mut c_void, u32, u32);

#[repr(C)]
union ID3D11DeviceContextVtable {
    raw: [usize; CONTEXT_SLOT_CAPACITY],
    context: ManuallyDrop<ID3D11DeviceContext_Vtbl>,
    context1: ManuallyDrop<ID3D11DeviceContext1_Vtbl>,
    context2: ManuallyDrop<ID3D11DeviceContext2_Vtbl>,
    context3: ManuallyDrop<ID3D11DeviceContext3_Vtbl>,
    context4: ManuallyDrop<ID3D11DeviceContext4_Vtbl>,
}

const _: () = assert!(size_of::<ID3D11DeviceContextVtable>() == CONTEXT_SLOT_CAPACITY * SLOT_SIZE);

impl ID3D11DeviceContextVtable {
    /// # Safety
    ///
    /// `original` must point to a vtable spanning at least `slots` entries.
    unsafe fn read(original: NonNull<usize>, slots: usize) -> Self {
        let mut cloned = Self {
            raw: [0; CONTEXT_SLOT_CAPACITY],
        };
        unsafe {
            std::ptr::copy_nonoverlapping(
                original.as_ptr().cast_const(),
                (&raw mut cloned.raw).cast::<usize>(),
                slots,
            );
        }
        cloned
    }

    fn table(&self) -> *const ID3D11DeviceContext_Vtbl {
        (&raw const self.context).cast()
    }

    fn table_mut(&mut self) -> *mut ID3D11DeviceContext_Vtbl {
        (&raw mut self.context).cast()
    }
}

struct ID3D11DeviceContextHook<A: Vtable> {
    /// Hook-patched clone installed as the object's vtable pointer.
    patched: Box<ID3D11DeviceContextVtable>,
    /// Verbatim copies of the original slots for downstream continuation.
    draw: DrawFn,
    draw_indexed: DrawIndexedFn,
    /// The interface generation cloned from the object's vtable.
    vtable: PhantomData<A>,
}

impl<A: Vtable> ID3D11DeviceContextHook<A> {
    /// # Safety
    ///
    /// `original` must be null or point to a vtable spanning at least
    /// [`A::SLOTS`](Vtable::SLOTS) entries.
    unsafe fn clone_from(original: *const usize) -> Option<Self> {
        let original = NonNull::new(original.cast_mut())?;
        let mut patched = Box::new(unsafe { ID3D11DeviceContextVtable::read(original, A::SLOTS) });
        let (draw, draw_indexed) = unsafe {
            let table = patched.table();
            ((*table).Draw, (*table).DrawIndexed)
        };
        // SAFETY: the slots written below were copied above, so each place
        // projection targets an initialized entry within the clone.
        unsafe {
            let table = patched.table_mut();
            (*table).Draw = draw_trampoline;
            (*table).DrawIndexed = draw_indexed_trampoline;
        }
        Some(Self {
            patched,
            draw,
            draw_indexed,
            vtable: PhantomData,
        })
    }

    fn patched_vtable(&self) -> *const usize {
        std::ptr::from_ref::<ID3D11DeviceContextVtable>(&self.patched).cast()
    }
}

/// The immediate context is always cloned at its widest generation; the D3D11
/// runtime implements every generation on one object.
type ImmediateContextHook = ID3D11DeviceContextHook<ID3D11DeviceContext4_Vtbl>;

static CONTEXTS: LazyLock<DashMap<usize, ImmediateContextHook>> = LazyLock::new(Default::default);

/// Installs draw interception on a device's immediate context so the producer
/// can track render regions.
///
/// # Safety
///
/// `output` must be null or point to a COM interface pointer for a live
/// `ID3D11DeviceContext`.
pub(crate) unsafe fn install_immediate_context(output: *mut *mut c_void) {
    if std::env::var_os("SNOWGLASS_TRACK_RENDER_TARGET").is_none() || output.is_null() {
        return;
    }
    let context = unsafe { *output };
    if context.is_null() {
        return;
    }
    let object_vtable = context.cast::<*const usize>();
    let current = unsafe { *object_vtable };
    if CONTEXTS
        .get(&(context as usize))
        .is_some_and(|state| state.patched_vtable() == current)
    {
        return;
    }
    let Some(vtable) = (unsafe { ImmediateContextHook::clone_from(current) }) else {
        return;
    };
    unsafe { *object_vtable = vtable.patched_vtable() };
    CONTEXTS.insert(context as usize, vtable);
}

unsafe extern "system" fn draw_trampoline(this: *mut c_void, vertex_count: u32, start_vertex: u32) {
    if let Some(original) = CONTEXTS.get(&(this as usize)).map(|state| state.draw) {
        unsafe { original(this, vertex_count, start_vertex) };
    }
    unsafe { snowglass_producer_d3d11::record_context_draw(this) };
}

unsafe extern "system" fn draw_indexed_trampoline(
    this: *mut c_void,
    index_count: u32,
    start_index: u32,
    base_vertex: i32,
) {
    if let Some(original) = CONTEXTS.get(&(this as usize)).map(|state| state.draw_indexed) {
        unsafe { original(this, index_count, start_index, base_vertex) };
    }
    unsafe { snowglass_producer_d3d11::record_context_draw(this) };
}
