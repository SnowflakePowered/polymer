use crate::SwapchainCopyEngine;

#[derive(Default)]
pub(super) struct RetiredCopies {
    pending: Vec<SwapchainCopyEngine>,
}
impl RetiredCopies {
    pub fn retire(&mut self, mut engine: SwapchainCopyEngine) {
        if !engine.ready_to_retire() {
            self.pending.push(engine);
        }
    }
    pub fn poll(&mut self) {
        self.pending.retain_mut(|engine| !engine.ready_to_retire());
    }
    pub fn wait(&mut self) {
        for engine in &mut self.pending {
            engine.wait_idle();
        }
        self.poll();
    }
}
impl Drop for RetiredCopies {
    fn drop(&mut self) {
        self.poll();
        // Keep native ownership if destruction occurs before GPU completion.
        for engine in self.pending.drain(..) {
            std::mem::forget(engine);
        }
    }
}
