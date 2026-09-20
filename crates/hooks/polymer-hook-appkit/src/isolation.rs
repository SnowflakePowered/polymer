use objc2::{
    ClassType, MainThreadMarker,
    runtime::{AnyObject, Bool, Imp, Sel},
    sel,
};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSWindow};
use polymer_platform_macos::spaces::PrivateSpace;
use std::{cell::RefCell, io, sync::OnceLock};

type Order = unsafe extern "C-unwind" fn(&NSWindow, Sel, *mut AnyObject);
type Update = unsafe extern "C-unwind" fn(&NSApplication, Sel);
type Policy =
    unsafe extern "C-unwind" fn(&NSApplication, Sel, NSApplicationActivationPolicy) -> Bool;
static ORDER: OnceLock<Order> = OnceLock::new();
static UPDATE: OnceLock<Update> = OnceLock::new();
static POLICY: OnceLock<Policy> = OnceLock::new();
thread_local! {
    static SPACE: RefCell<Option<PrivateSpace>> = const { RefCell::new(None) };
}

pub(super) struct Isolation;

impl Isolation {
    pub extern "C" fn initialize() {
        // Desktop isolation is explicitly enabled by the launcher. Injection alone
        // must not hide ordinary, non-isolated guest windows.
        if std::env::var_os("POLYMER_DESKTOP_ISOLATION").as_deref()
            != Some(std::ffi::OsStr::new("1"))
        {
            return;
        }
        if let Err(error) = Self::install() {
            eprintln!("Polymer desktop isolation failed: {error}");
            std::process::abort();
        }
    }

    fn install() -> io::Result<()> {
        let window = NSWindow::class();
        let app = NSApplication::class();
        let order = window.instance_method(sel!(orderFront:));
        let key = window.instance_method(sel!(makeKeyAndOrderFront:));
        let update = app.instance_method(sel!(updateWindows));
        let policy = app.instance_method(sel!(setActivationPolicy:));
        let activate = app.instance_method(sel!(activateIgnoringOtherApps:));
        let activate_new = app.instance_method(sel!(activate));
        let (
            Some(order),
            Some(key),
            Some(update),
            Some(policy),
            Some(activate),
            Some(activate_new),
        ) = (order, key, update, policy, activate, activate_new)
        else {
            return Err(io::Error::other(
                "required AppKit isolation selector is missing",
            ));
        };
        // SAFETY: each replacement and saved IMP has the exact selector ABI.
        // Installed by the image initializer before the guest's main runs.
        unsafe {
            ORDER
                .set(std::mem::transmute::<Imp, Order>(order.implementation()))
                .unwrap();
            UPDATE
                .set(std::mem::transmute::<Imp, Update>(update.implementation()))
                .unwrap();
            POLICY
                .set(std::mem::transmute::<Imp, Policy>(policy.implementation()))
                .unwrap();
            order.set_implementation(std::mem::transmute::<Order, Imp>(Self::order));
            update.set_implementation(std::mem::transmute::<Update, Imp>(Self::update));
            policy.set_implementation(std::mem::transmute::<Policy, Imp>(Self::policy));
            key.set_implementation(std::mem::transmute::<
                unsafe extern "C-unwind" fn(&NSWindow, Sel, *mut AnyObject),
                Imp,
            >(Self::make_key));
            activate.set_implementation(std::mem::transmute::<
                unsafe extern "C-unwind" fn(&NSApplication, Sel, Bool),
                Imp,
            >(Self::activate_ignoring));
            activate_new.set_implementation(std::mem::transmute::<Update, Imp>(Self::activate));
        }
        Ok(())
    }

    fn capture(window: &NSWindow) {
        let result = (|| {
            let main = MainThreadMarker::new()
                .ok_or_else(|| io::Error::other("AppKit window ordered off the main thread"))?;
            let app = NSApplication::sharedApplication(main);
            app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
            if app.activationPolicy() != NSApplicationActivationPolicy::Accessory {
                return Err(io::Error::other(
                    "guest rejected accessory activation policy",
                ));
            }
            let number = u32::try_from(window.windowNumber()).map_err(io::Error::other)?;
            SPACE.with(|slot| {
                let mut slot = slot.borrow_mut();
                if slot.is_none() {
                    *slot = Some(PrivateSpace::new(main)?);
                }
                slot.as_ref().unwrap().capture_window(number)
            })
        })();
        if let Err(error) = result {
            // Isolation was requested. Continuing visibly would violate it.
            eprintln!("Polymer desktop isolation failed: {error}");
            std::process::abort();
        }
    }

    unsafe extern "C-unwind" fn order(window: &NSWindow, selector: Sel, sender: *mut AnyObject) {
        unsafe { ORDER.get().unwrap()(window, selector, sender) };
        Self::capture(window);
    }

    unsafe extern "C-unwind" fn make_key(window: &NSWindow, _: Sel, sender: *mut AnyObject) {
        // Order normally so AX still sees it, without activating the guest.
        unsafe { window.orderFront(sender.as_ref()) };
        Self::capture(window);
    }

    unsafe extern "C-unwind" fn update(app: &NSApplication, selector: Sel) {
        unsafe { UPDATE.get().unwrap()(app, selector) };
        // AppKit has no public NSWindowDidBecomeVisibleNotification. Sweep after
        // updateWindows as well as intercepting the front-ordering selectors.
        for window in app.windows() {
            if window.isVisible() {
                Self::capture(&window);
            }
        }
    }

    unsafe extern "C-unwind" fn policy(
        app: &NSApplication,
        selector: Sel,
        _: NSApplicationActivationPolicy,
    ) -> Bool {
        unsafe { POLICY.get().unwrap()(app, selector, NSApplicationActivationPolicy::Accessory) }
    }
    unsafe extern "C-unwind" fn activate_ignoring(_: &NSApplication, _: Sel, _: Bool) {}
    unsafe extern "C-unwind" fn activate(_: &NSApplication, _: Sel) {}
}
