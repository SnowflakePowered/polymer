use parking_lot::{Mutex, RwLock};

static INLINE_HOOK_MUTATION: Mutex<()> = Mutex::new(());

/// A thread-safe owner for an `ib-hook` inline hook.
///
/// Hook state changes take an exclusive lock, while trampoline access only
/// takes a shared lock. This lets hook installers publish their hook storage
/// before enabling it, so a detour can always find its original trampoline.
#[derive(Debug)]
pub struct InlineDetour<F: ib_hook::FnPtr> {
    hook: RwLock<ib_hook::inline::InlineHook<F>>,
}

impl<F: ib_hook::FnPtr> InlineDetour<F> {
    pub const fn new(target: F, detour: F) -> Self {
        Self {
            hook: RwLock::new(ib_hook::inline::InlineHook::new(target, detour)),
        }
    }

    pub fn enable(&self) -> Result<(), i32> {
        let _mutation = INLINE_HOOK_MUTATION.lock();
        let mut hook = self.hook.write();
        let result = hook.enable();
        if result.is_ok() {
            Ok(())
        } else {
            Err(result.0)
        }
    }

    pub fn disable(&self) -> Result<(), i32> {
        let _mutation = INLINE_HOOK_MUTATION.lock();
        let mut hook = self.hook.write();
        let result = hook.disable();
        if result.is_ok() {
            Ok(())
        } else {
            Err(result.0)
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.hook.read().is_enabled()
    }

    pub fn trampoline(&self) -> F {
        self.hook.read().trampoline()
    }
}

impl<F: ib_hook::FnPtr> snowglass_hook::TerminalLifecycle for InlineDetour<F> {
    fn enable(&self) -> Result<(), i32> {
        InlineDetour::enable(self)
    }

    fn disable(&self) -> Result<(), i32> {
        InlineDetour::disable(self)
    }
}

impl<F: ib_hook::FnPtr> snowglass_hook::TerminalCallable for InlineDetour<F> {
    type Function = F;

    fn terminal_callable(&self) -> F {
        self.trampoline()
    }
}

impl<F: ib_hook::FnPtr> snowglass_hook::TerminalDetour<F> for InlineDetour<F> {
    fn create(target: F, detour: F) -> Self {
        Self::new(target, detour)
    }
}

/// Calls an installed detour's original trampoline, or returns `fallback` if
/// the detour is absent or disabled.
pub fn call_original<T, R>(
    detour: Option<&InlineDetour<T>>,
    fallback: R,
    call: impl FnOnce(&InlineDetour<T>) -> R,
) -> R
where
    T: ib_hook::FnPtr,
{
    match detour {
        Some(detour) if detour.is_enabled() => call(detour),
        Some(_) | None => fallback,
    }
}
