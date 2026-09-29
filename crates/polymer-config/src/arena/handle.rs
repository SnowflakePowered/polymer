//! Well-typed indices into [`Arena`]s.
//!
//! This module defines [`Handle`] and related types.
//!
//! [`Arena`]: super::Arena

use core::{cmp::Ordering, fmt, hash, marker::PhantomData};

/// A unique index in the arena array that a handle points to.
/// The "non-max" part ensures that an `Option<Handle<T>>` has
/// the same size and representation as `Handle<T>`.
pub type Index = crate::arena::non_max_u32::NonMaxU32;

/// A strongly typed reference to an arena item.
///
/// A `Handle` value can be used as an index into an [`Arena`].
///
/// [`Arena`]: super::Arena
pub struct Handle<T> {
    index: Index,
    marker: PhantomData<T>,
}

impl<T> Clone for Handle<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Handle<T> {}

impl<T> Default for Handle<T> {
    fn default() -> Self {
        Handle::from_usize(0)
    }
}

impl<T> PartialEq for Handle<T> {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index
    }
}

impl<T> Eq for Handle<T> {}

impl<T> PartialOrd for Handle<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for Handle<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.index.cmp(&other.index)
    }
}

impl<T> fmt::Debug for Handle<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        write!(formatter, "[{}]", self.index)
    }
}

impl<T> hash::Hash for Handle<T> {
    fn hash<H: hash::Hasher>(&self, hasher: &mut H) {
        self.index.hash(hasher)
    }
}

impl<T> Handle<T> {
    pub(crate) const fn new(index: Index) -> Self {
        Handle {
            index,
            marker: PhantomData,
        }
    }

    /// Returns the index of this handle.
    pub const fn index(self) -> usize {
        self.index.get() as usize
    }

    /// Convert a `usize` index into a `Handle<T>`.
    pub(crate) fn from_usize(index: usize) -> Self {
        let handle_index = u32::try_from(index)
            .ok()
            .and_then(Index::new)
            .expect("Failed to insert into arena. Handle overflows");
        Handle::new(handle_index)
    }

}
