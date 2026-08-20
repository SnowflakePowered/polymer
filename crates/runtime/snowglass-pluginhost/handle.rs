//! Opaque handles for resources owned by a plugin worker.

use std::{
    fmt,
    hash::{Hash, Hasher},
    marker::PhantomData,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

/// A typed token used at the boundary between JavaScript and Rust-owned state.
pub struct Handle<T: ?Sized> {
    raw: u64,
    marker: PhantomData<fn() -> T>,
}

impl<T: ?Sized> Handle<T> {
    pub fn new() -> Self {
        Self::from_raw(NEXT_HANDLE.fetch_add(1, Ordering::Relaxed))
    }

    pub const fn from_raw(raw: u64) -> Self {
        Self {
            raw,
            marker: PhantomData,
        }
    }

}

impl<T: ?Sized> Clone for Handle<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: ?Sized> Copy for Handle<T> {}

impl<T: ?Sized> PartialEq for Handle<T> {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl<T: ?Sized> Eq for Handle<T> {}

impl<T: ?Sized> Hash for Handle<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.raw.hash(state);
    }
}

impl<T: ?Sized> fmt::Debug for Handle<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Handle({:016x})", self.raw)
    }
}

impl<T: ?Sized> fmt::Display for Handle<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:016x}", self.raw)
    }
}

impl<T: ?Sized> Default for Handle<T> {
    fn default() -> Self {
        Self::new()
    }
}
