use super::{frontend::Frontend, tracker::Tracker};
use block2::{DynBlock, RcBlock};
use objc2::{
    AnyThread, DefinedClass, Message, define_class, msg_send, rc::Retained, runtime::ProtocolObject,
};
use objc2_foundation::{NSObject, NSObjectProtocol};
use objc2_metal::{MTLDrawable, MTLDrawablePresentedHandler, MTLTexture};
use objc2_quartz_core::{CACurrentMediaTime, CAMetalDrawable, CAMetalLayer};
use polymer_presentation::swapchain::AppLease;
use polymer_producer_metal::swapchain::TextureAccess;
use std::{
    ptr::NonNull,
    sync::{Arc, Mutex},
};

type Handler = dyn Fn(NonNull<ProtocolObject<dyn MTLDrawable>>);
struct State {
    lease: Option<AppLease>,
    scheduled: Vec<RcBlock<Handler>>,
    presented: Vec<RcBlock<Handler>>,
    time: f64,
    started: bool,
}
pub(super) struct Data {
    frontend: Arc<Frontend>,
    layer: Retained<CAMetalLayer>,
    access: TextureAccess,
    id: usize,
    state: Mutex<State>,
}
define_class!(
    #[unsafe(super(NSObject))]
    #[ivars = Data]
    pub(super) struct Drawable;
    unsafe impl NSObjectProtocol for Drawable {}
    unsafe impl CAMetalDrawable for Drawable {
        #[unsafe(method_id(texture))]
        fn texture(&self) -> Retained<ProtocolObject<dyn MTLTexture>> { self.ivars().access.texture.clone() }
        #[unsafe(method_id(layer))]
        fn layer(&self) -> Retained<CAMetalLayer> { self.ivars().layer.clone() }
    }
    unsafe impl MTLDrawable for Drawable {
        #[unsafe(method(present))]
        fn present(&self) { self.begin(); }
        #[unsafe(method(presentAtTime:))]
        fn present_at_time(&self, _: f64) { self.begin(); }
        #[unsafe(method(presentAfterMinimumDuration:))]
        fn present_after(&self, _: f64) { self.begin(); }
        #[unsafe(method(addPresentedHandler:))]
        unsafe fn add_presented(&self, block: MTLDrawablePresentedHandler) {
            if let Some(block) = unsafe { RcBlock::copy(block) } { self.add_handler(block, false); }
        }
        #[unsafe(method(presentedTime))]
        fn presented_time(&self) -> f64 { self.ivars().state.lock().unwrap().time }
        #[unsafe(method(drawableID))]
        fn drawable_id(&self) -> usize { self.ivars().id }
    }
    impl Drawable {
        // Metal's private scheduling callback is essential for command buffers to retire.
        #[unsafe(method(addPresentScheduledHandler:))]
        fn scheduled(&self, block: &DynBlock<Handler>) { self.add_handler(block.copy(), true); }
    }
);
pub(super) struct Job(
    pub Retained<Drawable>,
    pub AppLease,
    pub polymer_producer_metal::swapchain::PresentWork,
);
// SAFETY: Metal callbacks may run on arbitrary driver threads. The drawable's
// mutable bookkeeping is locked; its native resource references remain retained.
unsafe impl Send for Job {}
impl Drawable {
    pub fn new(
        frontend: Arc<Frontend>,
        layer: &CAMetalLayer,
        access: TextureAccess,
        lease: AppLease,
        id: usize,
    ) -> Retained<Self> {
        let value = Self::alloc().set_ivars(Data {
            frontend,
            layer: layer.retain(),
            access,
            id,
            state: Mutex::new(State {
                lease: Some(lease),
                scheduled: Vec::new(),
                presented: Vec::new(),
                time: 0.0,
                started: false,
            }),
        });
        unsafe { msg_send![super(value), init] }
    }
    fn callback(&self, block: &DynBlock<Handler>) {
        block.call((NonNull::from(ProtocolObject::from_ref(self)),));
    }
    fn add_handler(&self, block: RcBlock<Handler>, scheduled: bool) {
        let mut state = self.ivars().state.lock().unwrap();
        if (scheduled && state.started) || (!scheduled && state.time != 0.0) {
            drop(state);
            self.callback(&block);
        } else if scheduled {
            state.scheduled.push(block);
        } else {
            state.presented.push(block);
        }
    }
    fn begin(&self) {
        let (lease, handlers) = {
            let mut state = self.ivars().state.lock().unwrap();
            let Some(lease) = state.lease.take() else {
                return;
            };
            state.started = true;
            (lease, std::mem::take(&mut state.scheduled))
        };
        for block in handlers {
            self.callback(&block);
        }
        let job = Job(self.retain(), lease, Tracker::submitted());
        if let Err(error) = self.ivars().frontend.present(job) {
            eprintln!("Polymer Metal present failed: {error}");
        }
    }
    pub fn complete(&self) {
        let handlers = {
            let mut state = self.ivars().state.lock().unwrap();
            state.time = CACurrentMediaTime();
            std::mem::take(&mut state.presented)
        };
        for block in handlers {
            self.callback(&block);
        }
    }
}
impl Drop for Data {
    fn drop(&mut self) {
        if let Some(lease) = self.state.get_mut().unwrap().lease.take() {
            self.frontend.cancel(lease);
        }
    }
}
