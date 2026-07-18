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

/// The layout-erased view of a hooked immediate context stored in the
/// registry. `Draw`/`DrawIndexed` live in the base `ID3D11DeviceContext`
/// layout common to every generation, so the exposed continuations do not
/// depend on the cloned width.
trait ContextHook: Send + Sync {
    /// The vtable pointer that intercepts a hooked object.
    fn patched_vtable(&self) -> *const usize;
    fn draw(&self) -> DrawFn;
    fn draw_indexed(&self) -> DrawIndexedFn;
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

// SAFETY: the clone holds only fn pointers and copied vtable slots; the
// `PhantomData<A>` layout marker carries no data. Matches the other
// per-object hooks.
unsafe impl<A: Vtable> Send for ID3D11DeviceContextHook<A> {}
unsafe impl<A: Vtable> Sync for ID3D11DeviceContextHook<A> {}

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
}

impl<A: Vtable> ContextHook for ID3D11DeviceContextHook<A> {
    fn patched_vtable(&self) -> *const usize {
        std::ptr::from_ref::<ID3D11DeviceContextVtable>(&self.patched).cast()
    }

    fn draw(&self) -> DrawFn {
        self.draw
    }

    fn draw_indexed(&self) -> DrawIndexedFn {
        self.draw_indexed
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
    // The immediate context implements one linear interface generation; cloning
    // at a wider layout than the object supports would read past its real
    // vtable. Probe the widest generation and clone at exactly that width.
    let Some(install) = widest_context(context) else {
        return;
    };
    unsafe { install(context) };
}

/// The widest device-context interface implemented by the object, paired with
/// the install routine for that layout's clone width.
fn widest_context(context: *mut c_void) -> Option<InstallFn> {
    let unknown = unsafe { IUnknown::from_raw_borrowed(&context) }?;
    macro_rules! try_cast {
        ($interface:ty, $vtbl:ty) => {
            if unknown.cast::<$interface>().is_ok() {
                return Some(install_context_kind::<$vtbl> as InstallFn);
            }
        };
    }
    try_cast!(ID3D11DeviceContext4, ID3D11DeviceContext4_Vtbl);
    try_cast!(ID3D11DeviceContext3, ID3D11DeviceContext3_Vtbl);
    try_cast!(ID3D11DeviceContext2, ID3D11DeviceContext2_Vtbl);
    try_cast!(ID3D11DeviceContext1, ID3D11DeviceContext1_Vtbl);
    try_cast!(ID3D11DeviceContext, ID3D11DeviceContext_Vtbl);
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
