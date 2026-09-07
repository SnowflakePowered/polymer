use super::AcquireDeadline;
use std::{
    marker::PhantomData,
    ops::{Deref, DerefMut},
    rc::Rc,
    sync::{Condvar, Mutex},
    thread::ThreadId,
    time::Instant,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OwnerUnavailable {
    Reentrant,
    Busy,
    Abandoned,
}

struct OwnerState<T> {
    value: Option<T>,
    caller: Option<ThreadId>,
}

/// A reentrant hook can observe Busy without holding a lock across any native
/// call. T's Send/Sync/affinity remains intact, including context-local GL T.
pub struct OwnerCell<T> {
    value: Mutex<OwnerState<T>>,
    returned: Condvar,
}
impl<T> OwnerCell<T> {
    pub fn new(value: T) -> Self {
        Self {
            value: Mutex::new(OwnerState {
                value: Some(value),
                caller: None,
            }),
            returned: Condvar::new(),
        }
    }
    pub fn enter(&self) -> Option<OwnerCall<'_, T>> {
        self.enter_until(AcquireDeadline::Nonblocking).ok()
    }
    /// Waits only for a different thread's call. Reentry on the current thread
    /// cannot make progress by waiting, so it returns immediately. The mutex is
    /// released while waiting and before the caller receives the native owner.
    pub fn enter_until(
        &self,
        deadline: AcquireDeadline,
    ) -> Result<OwnerCall<'_, T>, OwnerUnavailable> {
        let caller = std::thread::current().id();
        let mut state = self.value.lock().map_err(|_| OwnerUnavailable::Abandoned)?;
        loop {
            if let Some(value) = state.value.take() {
                state.caller = Some(caller);
                return Ok(OwnerCall {
                    owner: self,
                    value: Some(value),
                    thread_bound: PhantomData,
                });
            }
            match state.caller {
                Some(owner) if owner == caller => return Err(OwnerUnavailable::Reentrant),
                None => return Err(OwnerUnavailable::Abandoned),
                _ => {}
            }
            let AcquireDeadline::Until(until) = deadline else {
                return Err(OwnerUnavailable::Busy);
            };
            let remaining = until.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(OwnerUnavailable::Busy);
            }
            state = self
                .returned
                .wait_timeout(state, remaining)
                .map_err(|_| OwnerUnavailable::Abandoned)?
                .0;
        }
    }
    pub fn into_inner(self) -> Option<T> {
        self.value.into_inner().ok()?.value
    }
}
pub struct OwnerCall<'a, T> {
    owner: &'a OwnerCell<T>,
    value: Option<T>,
    thread_bound: PhantomData<Rc<()>>,
}
impl<T> Deref for OwnerCall<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.value.as_ref().unwrap()
    }
}
impl<T> DerefMut for OwnerCall<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.value.as_mut().unwrap()
    }
}
impl<T> Drop for OwnerCall<'_, T> {
    fn drop(&mut self) {
        if let Ok(mut destination) = self.owner.value.lock() {
            if !std::thread::panicking() {
                destination.value = self.value.take();
            }
            destination.caller = None;
            self.owner.returned.notify_all();
        }
    }
}
