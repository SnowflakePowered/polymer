use std::{
    collections::VecDeque,
    io,
    sync::{Condvar, Mutex},
    time::{Duration, Instant},
};

pub(super) struct Queue<T> {
    state: Mutex<(VecDeque<T>, bool)>,
    ready: Condvar,
}
impl<T> Default for Queue<T> {
    fn default() -> Self {
        Self {
            state: Mutex::new((VecDeque::new(), false)),
            ready: Condvar::new(),
        }
    }
}
impl<T> Queue<T> {
    pub fn push(&self, value: T) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.1 || state.0.len() >= 64 {
            return false;
        }
        state.0.push_back(value);
        self.ready.notify_one();
        true
    }
    pub fn receive(&self, timeout: Option<Duration>) -> io::Result<T> {
        let deadline = timeout.map(|timeout| Instant::now() + timeout);
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(value) = state.0.pop_front() {
                return Ok(value);
            }
            if state.1 {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
            }
            state = match deadline {
                None => self.ready.wait(state).unwrap(),
                Some(deadline) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Err(io::Error::from(io::ErrorKind::TimedOut));
                    }
                    self.ready.wait_timeout(state, remaining).unwrap().0
                }
            };
        }
    }
    pub fn close(&self) {
        self.state.lock().unwrap().1 = true;
        self.ready.notify_all();
    }
    pub fn discard(&self) {
        let queued = {
            let mut state = self.state.lock().unwrap();
            state.1 = true;
            std::mem::take(&mut state.0)
        };
        self.ready.notify_all();
        drop(queued);
    }
    pub fn closed(&self) -> bool {
        self.state.lock().unwrap().1
    }
}
