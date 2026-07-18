use dashmap::DashMap;
use std::ffi::c_void;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ptr::NonNull;
use std::sync::LazyLock;
use windows::{
    Win32::Graphics::Direct3D11::{
        ID3D11DeviceContext, ID3D11DeviceContext_Vtbl, ID3D11DeviceContext1,
        ID3D11DeviceContext1_Vtbl, ID3D11DeviceContext2, ID3D11DeviceContext2_Vtbl,
        ID3D11DeviceContext3, ID3D11DeviceContext3_Vtbl, ID3D11DeviceContext4,
        ID3D11DeviceContext4_Vtbl,
    },
    core::{IUnknown, Interface},
};

snowglass_hook::vtable! {
    [
        ID3D11DeviceContext_Vtbl,
        ID3D11DeviceContext1_Vtbl,
        ID3D11DeviceContext2_Vtbl,
        ID3D11DeviceContext3_Vtbl,
        ID3D11DeviceContext4_Vtbl,
    ],
}

const SLOT_SIZE: usize = size_of::<usize>();
const CONTEXT_SLOT_CAPACITY: usize = <ID3D11DeviceContext4_Vtbl as Vtable>::SLOTS;

type DrawIndexedFn = unsafe extern "system" fn(*mut c_void, u32, u32, i32);
type DrawFn = unsafe extern "system" fn(*mut c_void, u32, u32);
type InstallFn = unsafe fn(*mut c_void) -> bool;

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
    unsafe fn read(original: NonNull<c_void>, slots: usize) -> Self {
        let mut cloned = Self {
            raw: [0; CONTEXT_SLOT_CAPACITY],
        };
        unsafe {
            std::ptr::copy_nonoverlapping(
                original.as_ptr().cast::<usize>().cast_const(),
                (&raw mut cloned.raw).cast::<usize>(),
                slots,
            );
        }
        cloned
    }

    fn table_mut(&mut self) -> *mut ID3D11DeviceContext_Vtbl {
        (&raw mut self.context).cast()
    }
}

/// The layout-erased view of a hooked immediate context stored in the
/// registry. `Draw`/`DrawIndexed` live in the base `ID3D11DeviceContext`
/// layout common to every generation, so the exposed continuations do not
/// depend on the cloned width.
trait ContextHook: snowglass_hook::VtableHook {
    fn draw(&self) -> DrawFn;
    fn draw_indexed(&self) -> DrawIndexedFn;
}

struct ID3D11DeviceContextHook<A: Vtable> {
    /// Hook-patched clone installed as the object's vtable pointer.
    patched: Box<ID3D11DeviceContextVtable>,
    /// Verbatim copy of the original slots for downstream continuation.
    saved: ID3D11DeviceContextVtable,
    /// The object's original vtable pointer.
    original: NonNull<c_void>,
    /// The interface generation cloned from the object's vtable.
    vtable: PhantomData<A>,
}

snowglass_hook::impl_com_hook! {
    impl ContextHook for ID3D11DeviceContextHook {
        vtable ID3D11DeviceContextVtable;
        patch |patched| {
        // SAFETY: the slots written below were copied above, so each place
        // projection targets an initialized entry within the clone.
        unsafe {
            let table = patched.table_mut();
            (*table).Draw = draw_trampoline;
            (*table).DrawIndexed = draw_indexed_trampoline;
        }
        }

        #[vtable = ID3D11DeviceContext_Vtbl]
        #[base = Draw]
        fn draw(&self) -> DrawFn;
        #[vtable = ID3D11DeviceContext_Vtbl]
        #[base = DrawIndexed]
        fn draw_indexed(&self) -> DrawIndexedFn;
    }
}

static CONTEXTS: LazyLock<DashMap<usize, Box<dyn ContextHook>>> = LazyLock::new(Default::default);

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
    let current = unsafe { *context.cast::<*const usize>() };
    if current.is_null() {
        return;
    }
    if CONTEXTS
        .get(&(context as usize))
        .is_some_and(|state| state.patched_vtable() == current)
    {
        return;
    }
    let Some(install) = widest_context(context) else {
        return;
    };
    unsafe { install(context) };
}

fn widest_context(context: *mut c_void) -> Option<InstallFn> {
    let unknown = unsafe { IUnknown::from_raw_borrowed(&context) }?;
    macro_rules! try_interface {
        ($interface:ty, $vtbl:ty) => {
            if unknown.cast::<$interface>().is_ok() {
                return Some(install_context_kind::<$vtbl> as InstallFn);
            }
        };
    }
    try_interface!(ID3D11DeviceContext4, ID3D11DeviceContext4_Vtbl);
    try_interface!(ID3D11DeviceContext3, ID3D11DeviceContext3_Vtbl);
    try_interface!(ID3D11DeviceContext2, ID3D11DeviceContext2_Vtbl);
    try_interface!(ID3D11DeviceContext1, ID3D11DeviceContext1_Vtbl);
    try_interface!(ID3D11DeviceContext, ID3D11DeviceContext_Vtbl);
    None
}

unsafe fn install_context_kind<A: Vtable>(context: *mut c_void) -> bool {
    if context.is_null() {
        return false;
    }
    let object_vtable = context.cast::<*const usize>();
    let current = unsafe { *object_vtable };
    let Some(hook) = (unsafe { ID3D11DeviceContextHook::<A>::clone_from(current) }) else {
        return false;
    };
    let state: Box<dyn ContextHook> = Box::new(hook);
    unsafe { *object_vtable = state.patched_vtable() };
    CONTEXTS.insert(context as usize, state);
    true
}

unsafe extern "system" fn draw_trampoline(this: *mut c_void, vertex_count: u32, start_vertex: u32) {
    if let Some(original) = CONTEXTS.get(&(this as usize)).map(|state| state.draw()) {
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
    if let Some(original) = CONTEXTS
        .get(&(this as usize))
        .map(|state| state.draw_indexed())
    {
        unsafe { original(this, index_count, start_index, base_vertex) };
    }
    unsafe { snowglass_producer_d3d11::record_context_draw(this) };
}
