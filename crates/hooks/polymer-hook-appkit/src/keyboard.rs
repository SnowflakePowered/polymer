use std::sync::atomic::{AtomicBool, Ordering};

static KEYS: [AtomicBool; 128] = [const { AtomicBool::new(false) }; 128];
static ATTACHED: AtomicBool = AtomicBool::new(false);

pub(super) struct Keyboard;
impl Keyboard {
    pub fn set(code: u16, pressed: bool) {
        if let Some(key) = KEYS.get(usize::from(code)) {
            key.store(pressed, Ordering::Release);
        }
    }
    pub fn attach() {
        ATTACHED.store(true, Ordering::Release);
    }
    pub fn clear() {
        for key in &KEYS {
            key.store(false, Ordering::Release);
        }
    }
    unsafe extern "C" fn state(source: i32, code: u16) -> bool {
        if ATTACHED.load(Ordering::Acquire) {
            return KEYS
                .get(usize::from(code))
                .is_some_and(|key| key.load(Ordering::Acquire));
        }
        unsafe { CGEventSourceKeyState(source, code) }
    }
}

unsafe extern "C" {
    fn CGEventSourceKeyState(source: i32, code: u16) -> bool;
}

// dyld interposes imports in the guest, leaving this image's original call intact.
#[repr(C)]
struct Interpose {
    replacement: unsafe extern "C" fn(i32, u16) -> bool,
    original: unsafe extern "C" fn(i32, u16) -> bool,
}
#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static KEY_STATE: Interpose = Interpose {
    replacement: Keyboard::state,
    original: CGEventSourceKeyState,
};
