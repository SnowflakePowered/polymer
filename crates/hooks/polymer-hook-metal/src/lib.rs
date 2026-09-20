//! CAMetalLayer frontend for Polymer's shared swapchain controller.
#![cfg(target_os = "macos")]
mod drawable;
mod frontend;
mod tracker;
use frontend::Frontend;
use objc2::{
    AnyThread, ClassType, DefinedClass, MainThreadMarker, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, Imp, ProtocolObject, Sel},
    sel,
};
use objc2_app_kit::{NSApplication, NSView};
use objc2_foundation::{NSObject, NSObjectProtocol, NSSize};
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};
use polymer_presentation::swapchain::{Candidate, OwnerCell, SourceIdentity, WindowIdentity};
use std::{
    ffi::c_void,
    io,
    sync::{Arc, Mutex, OnceLock},
};

type Next =
    unsafe extern "C-unwind" fn(&CAMetalLayer, Sel) -> *mut ProtocolObject<dyn CAMetalDrawable>;
static ORIGINAL: OnceLock<Next> = OnceLock::new();
type SetSize = unsafe extern "C-unwind" fn(&CAMetalLayer, Sel, NSSize);
static SET_SIZE: OnceLock<SetSize> = OnceLock::new();
static ASSOCIATION: u8 = 0;
static ATTACH: Mutex<()> = Mutex::new(());
type UpdateWindows = unsafe extern "C-unwind" fn(&NSApplication, Sel);
static UPDATE: OnceLock<UpdateWindows> = OnceLock::new();
struct LayerState {
    frontend: OwnerCell<Option<Arc<Frontend>>>,
    window: Mutex<Option<(u64, String)>>,
}
define_class!(
    #[unsafe(super(NSObject))]
    #[ivars = LayerState]
    struct LayerOwner;
    unsafe impl NSObjectProtocol for LayerOwner {}
);
struct Hook;
impl Hook {
    extern "C" fn initialize() {
        if std::env::var_os("POLYMER_SESSION").is_none() {
            return;
        }
        if let Err(error) = Self::install() {
            eprintln!("Polymer Metal hook failed: {error}");
            std::process::abort();
        }
    }
    fn install() -> io::Result<()> {
        Frontend::initialize()?;
        let method = CAMetalLayer::class()
            .instance_method(sel!(nextDrawable))
            .ok_or_else(|| io::Error::other("CAMetalLayer nextDrawable missing"))?;
        unsafe {
            ORIGINAL
                .set(std::mem::transmute::<Imp, Next>(method.implementation()))
                .map_err(|_| io::Error::other("Metal hook already installed"))?;
            method.set_implementation(std::mem::transmute::<Next, Imp>(Self::next));
        }
        let method = CAMetalLayer::class()
            .instance_method(sel!(setDrawableSize:))
            .ok_or_else(|| io::Error::other("CAMetalLayer setDrawableSize missing"))?;
        unsafe {
            SET_SIZE
                .set(std::mem::transmute::<Imp, SetSize>(method.implementation()))
                .map_err(|_| io::Error::other("Metal size hook already installed"))?;
            method.set_implementation(std::mem::transmute::<SetSize, Imp>(Self::set_size));
        }
        let method = NSApplication::class()
            .instance_method(sel!(updateWindows))
            .ok_or_else(|| io::Error::other("AppKit updateWindows missing"))?;
        unsafe {
            UPDATE
                .set(std::mem::transmute::<Imp, UpdateWindows>(
                    method.implementation(),
                ))
                .map_err(|_| io::Error::other("window hook already installed"))?;
            method.set_implementation(std::mem::transmute::<UpdateWindows, Imp>(
                Self::update_windows,
            ));
        }
        Ok(())
    }
    unsafe extern "C-unwind" fn set_size(layer: &CAMetalLayer, selector: Sel, size: NSSize) {
        // A captured layer keeps the extent reserved by the shared assignment.
        // Guest window resizes change presentation, never the exported images.
        let state = Self::association(layer);
        let assigned = state
            .ivars()
            .frontend
            .enter()
            .is_none_or(|owner| owner.is_some());
        if !assigned {
            unsafe {
                SET_SIZE.get().unwrap()(layer, selector, size);
            }
        }
    }
    unsafe extern "C-unwind" fn update_windows(app: &NSApplication, selector: Sel) {
        unsafe {
            UPDATE.get().unwrap()(app, selector);
        }
        for window in app.windows() {
            if let Some(view) = window.contentView() {
                Self::associate_views(
                    &view,
                    window.windowNumber() as u64,
                    &window.title().to_string(),
                );
            }
        }
    }
    fn associate_views(view: &NSView, window: u64, title: &str) {
        if let Some(layer) = view
            .layer()
            .and_then(|layer| layer.downcast::<CAMetalLayer>().ok())
        {
            *Self::association(&layer).ivars().window.lock().unwrap() =
                Some((window, title.to_owned()));
        }
        for child in view.subviews() {
            Self::associate_views(&child, window, title);
        }
    }
    unsafe extern "C-unwind" fn next(
        layer: &CAMetalLayer,
        selector: Sel,
    ) -> *mut ProtocolObject<dyn CAMetalDrawable> {
        let result = Self::frontend(layer)
            .and_then(|frontend| frontend.map(|value| value.next(layer)).transpose());
        match result {
            Ok(Some(drawable)) => Retained::autorelease_return(drawable),
            Ok(None) => unsafe { ORIGINAL.get().unwrap()(layer, selector) },
            Err(error) => {
                eprintln!("Polymer Metal nextDrawable: {error}");
                std::ptr::null_mut()
            }
        }
    }
    fn association(layer: &CAMetalLayer) -> Retained<LayerOwner> {
        // Association retains the controller for exactly this layer lifetime.
        let object = {
            let _guard = ATTACH.lock().unwrap();
            let existing = unsafe {
                objc_getAssociatedObject(layer, &ASSOCIATION as *const _ as *const c_void)
            };
            if let Some(existing) = unsafe { Retained::<LayerOwner>::retain(existing.cast()) } {
                existing
            } else {
                let value = LayerOwner::alloc().set_ivars(LayerState {
                    frontend: OwnerCell::new(None),
                    window: Mutex::new(None),
                });
                let value: Retained<LayerOwner> = unsafe { msg_send![super(value), init] };
                unsafe {
                    objc_setAssociatedObject(
                        layer,
                        &ASSOCIATION as *const _ as *const c_void,
                        &value,
                        1,
                    );
                }
                value
            }
        };
        object
    }
    fn frontend(layer: &CAMetalLayer) -> io::Result<Option<Arc<Frontend>>> {
        let object = Self::association(layer);
        let mut owner = object
            .ivars()
            .frontend
            .enter()
            .ok_or_else(|| io::Error::from(io::ErrorKind::WouldBlock))?;
        if owner.is_none() {
            *owner = Frontend::create(layer)?;
        }
        Ok(owner.clone())
    }

    fn candidate(layer: &CAMetalLayer, source: SourceIdentity) -> Candidate {
        let window = MainThreadMarker::new()
            .and_then(|_| layer.delegate())
            .and_then(|delegate| {
                {
                    let object: &AnyObject = delegate.as_ref();
                    object.downcast_ref::<NSView>()
                }
                .and_then(NSView::window)
            });
        let metadata = window
            .map(|window| (window.windowNumber() as u64, window.title().to_string()))
            .or_else(|| {
                Self::association(layer)
                    .ivars()
                    .window
                    .lock()
                    .unwrap()
                    .clone()
            });
        let (handle, title) = metadata.unwrap_or_default();
        Candidate {
            source,
            window: WindowIdentity {
                handle,
                lifetime: source.lifetime,
            },
            title,
        }
    }
}
unsafe extern "C" {
    fn objc_getAssociatedObject(object: &CAMetalLayer, key: *const c_void) -> *mut AnyObject;
    fn objc_setAssociatedObject(
        object: &CAMetalLayer,
        key: *const c_void,
        value: &AnyObject,
        policy: usize,
    );
}
#[used]
#[unsafe(link_section = "__DATA,__mod_init_func")]
static INITIALIZE: extern "C" fn() = Hook::initialize;
