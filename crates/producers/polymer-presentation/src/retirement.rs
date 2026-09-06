//! Retirement decisions run on the caller's thread/context. Backends retain
//! native ownership until their synchronization permits destruction.
pub trait Retirable {
    fn ready_to_retire(&mut self) -> bool;
    /// Optional native wait used only at an explicit destruction boundary.
    /// Ordinary polling never invokes this potentially blocking operation.
    fn wait_for_retirement(&mut self) {}
}
pub struct RetirementQueue<B: Retirable> {
    pending: Vec<B>,
}
impl<B: Retirable> Default for RetirementQueue<B> {
    fn default() -> Self {
        Self {
            pending: Vec::new(),
        }
    }
}
impl<B: Retirable> RetirementQueue<B> {
    pub fn retire(&mut self, mut backend: B) {
        if !backend.ready_to_retire() {
            self.pending.push(backend);
        }
    }
    pub fn poll(&mut self) {
        self.pending
            .retain_mut(|backend| !backend.ready_to_retire());
    }
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
    pub fn wait(&mut self) {
        for backend in &mut self.pending {
            backend.wait_for_retirement();
        }
        self.poll();
    }
}
impl<B: Retirable> Drop for RetirementQueue<B> {
    fn drop(&mut self) {
        // Completion may have advanced since the owner's final poll. Release
        // ready backends on this thread without adding a shutdown wait.
        self.poll();
        // The owner should keep polling until empty. If it disappears first,
        // retain native ownership rather than destroy possibly in-use resources.
        for backend in self.pending.drain(..) {
            std::mem::forget(backend);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};
    struct Backend {
        complete: Rc<Cell<bool>>,
        destroyed: Rc<Cell<usize>>,
    }
    impl Retirable for Backend {
        fn ready_to_retire(&mut self) -> bool {
            self.complete.get()
        }
        fn wait_for_retirement(&mut self) {
            self.complete.set(true);
        }
    }
    impl Drop for Backend {
        fn drop(&mut self) {
            assert!(self.complete.get());
            self.destroyed.set(self.destroyed.get() + 1);
        }
    }
    #[test]
    fn one_busy_backend_does_not_hold_completed_siblings() {
        let complete = Rc::new(Cell::new(false));
        let destroyed = Rc::new(Cell::new(0));
        let mut queue = RetirementQueue::default();
        queue.retire(Backend {
            complete: complete.clone(),
            destroyed: destroyed.clone(),
        });
        queue.retire(Backend {
            complete: Rc::new(Cell::new(true)),
            destroyed: destroyed.clone(),
        });
        queue.poll();
        assert_eq!(destroyed.get(), 1);
        assert!(!queue.is_empty());
        complete.set(true);
        queue.poll();
        assert_eq!(destroyed.get(), 2);
        assert!(queue.is_empty());
        queue.retire(Backend {
            complete: Rc::new(Cell::new(false)),
            destroyed: destroyed.clone(),
        });
        queue.wait();
        assert_eq!(destroyed.get(), 3);
        assert!(queue.is_empty());
    }

    #[test]
    fn destruction_rechecks_completion_without_waiting_for_busy_siblings() {
        let finished = Rc::new(Cell::new(false));
        let busy = Rc::new(Cell::new(false));
        let destroyed = Rc::new(Cell::new(0));
        let mut queue = RetirementQueue::default();
        for complete in [&finished, &busy] {
            queue.retire(Backend {
                complete: complete.clone(),
                destroyed: destroyed.clone(),
            });
        }
        finished.set(true);
        drop(queue);
        assert_eq!(destroyed.get(), 1);
        // Backend::wait_for_retirement would set this flag, so it also proves
        // queue destruction did not block waiting for the still-busy backend.
        assert!(!busy.get());
    }
}
