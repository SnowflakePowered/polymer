use super::*;
use objc2::runtime::Method;
use polymer_hook::macos::{ObjcMethod, ObjcSwizzle};

pub(super) struct NextDrawable;
pub(super) struct DrawableSize;
pub(super) struct Windows;

// SAFETY: These methods are implemented by these classes and the signatures
// below match CAMetalLayer and NSApplication's Objective-C ABIs.
unsafe impl ObjcMethod<Next> for NextDrawable {
    fn method() -> &'static Method {
        CAMetalLayer::class()
            .instance_method(sel!(nextDrawable))
            .expect("nextDrawable missing")
    }
}
unsafe impl ObjcMethod<SetSize> for DrawableSize {
    fn method() -> &'static Method {
        CAMetalLayer::class()
            .instance_method(sel!(setDrawableSize:))
            .expect("setDrawableSize missing")
    }
}
unsafe impl ObjcMethod<UpdateWindows> for Windows {
    fn method() -> &'static Method {
        NSApplication::class()
            .instance_method(sel!(updateWindows))
            .expect("updateWindows missing")
    }
}

polymer_hook::chain_hooks! {
    #![order(reverse)]
    #![cfg_attr(target_os = "macos", detour(ObjcSwizzle<_, NextDrawable>))]
    #[installer]
    pub(super) struct DrawableInstallation;
    #[trampoline]
    #[terminal(installed(target = ObjcSwizzle::<Next, NextDrawable>::target()), fallback = std::ptr::null_mut())]
    #[infix(Hook::next)]
    pub(super) unsafe extern "C-unwind" fn next_drawable(layer: &CAMetalLayer, selector: Sel) -> *mut ProtocolObject<dyn CAMetalDrawable>;
}
polymer_hook::chain_hooks! {
    #![order(reverse)]
    #![cfg_attr(target_os = "macos", detour(ObjcSwizzle<_, DrawableSize>))]
    #[installer]
    pub(super) struct SizeInstallation;
    #[trampoline]
    #[terminal(installed(target = ObjcSwizzle::<SetSize, DrawableSize>::target()), fallback = ())]
    #[infix(Hook::set_size)]
    pub(super) unsafe extern "C-unwind" fn set_size(layer: &CAMetalLayer, selector: Sel, size: NSSize);
}
polymer_hook::chain_hooks! {
    #![order(reverse)]
    #![cfg_attr(target_os = "macos", detour(ObjcSwizzle<_, Windows>))]
    #[installer]
    pub(super) struct WindowsInstallation;
    #[trampoline]
    #[terminal(installed(target = ObjcSwizzle::<UpdateWindows, Windows>::target()), fallback = ())]
    #[postfix(Hook::update_windows(app, selector, result))]
    pub(super) unsafe extern "C-unwind" fn update_windows(app: &NSApplication, selector: Sel);
}
