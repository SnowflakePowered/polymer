use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct AclCredits {
    capacity: u16,
    outstanding: HashMap<u16, u16>,
}

impl AclCredits {
    pub fn new(capacity: u16) -> Self {
        Self {
            capacity,
            outstanding: HashMap::new(),
        }
    }
    pub fn capacity(&self) -> u16 {
        self.capacity
    }
    pub fn set_capacity(&mut self, capacity: u16) {
        self.capacity = capacity;
    }
    pub fn outstanding(&self) -> u16 {
        self.outstanding.values().copied().sum()
    }
    pub fn available(&self) -> u16 {
        self.capacity.saturating_sub(self.outstanding())
    }

    pub fn try_acquire(&mut self, handle: u16) -> bool {
        if self.available() == 0 {
            return false;
        }
        *self.outstanding.entry(handle & 0x0fff).or_default() += 1;
        true
    }

    pub fn complete(&mut self, handle: u16, count: u16) -> u16 {
        let handle = handle & 0x0fff;
        let Some(current) = self.outstanding.get_mut(&handle) else {
            return 0;
        };
        let released = (*current).min(count);
        *current -= released;
        if *current == 0 {
            self.outstanding.remove(&handle);
        }
        released
    }

    pub fn forget_handle(&mut self, handle: u16) -> u16 {
        self.outstanding.remove(&(handle & 0x0fff)).unwrap_or(0)
    }
}
