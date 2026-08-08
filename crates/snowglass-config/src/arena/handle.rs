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

#[derive(Clone, Copy, Debug, thiserror::Error, PartialEq)]
#[error("Handle {index} of {kind} is either not present, or inaccessible yet")]
pub struct BadHandle {
    pub kind: &'static str,
    pub _source: &'static str,
    pub index: usize,
}

impl BadHandle {
    pub fn new<T, H>(handle: Handle<T, H>) -> Self {
        Self {
            kind: core::any::type_name::<T>(),
            _source: core::any::type_name::<H>(),
            index: handle.index(),
        }
    }
}

/// A strongly typed reference to an arena item.
///
/// A `Handle` value can be used as an index into an [`Arena`].
///
/// [`Arena`]: super::Arena
pub struct Handle<T, D = ()> {
    index: Index,
    marker: PhantomData<T>,
    _source: PhantomData<D>,
}

impl<T, D> Clone for Handle<T, D> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T, D> Copy for Handle<T, D> {}

impl<T, D> Default for Handle<T, D> {
    fn default() -> Self {
        Handle::from_usize(0)
    }
}

impl<T, D> PartialEq for Handle<T, D> {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index
    }
}

impl<T, D> Eq for Handle<T, D> {}

impl<T, D> PartialOrd for Handle<T, D> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T, D> Ord for Handle<T, D> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.index.cmp(&other.index)
    }
}

impl<T, D> fmt::Debug for Handle<T, D> {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        write!(formatter, "[{}]", self.index)
    }
}

impl<T, D> hash::Hash for Handle<T, D> {
    fn hash<H: hash::Hasher>(&self, hasher: &mut H) {
        self.index.hash(hasher)
    }
}

impl<T, D> Handle<T, D> {
    pub(crate) const fn new(index: Index) -> Self {
        Handle {
            index,
            marker: PhantomData,
            _source: PhantomData,
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

    /// Write this handle's index to `formatter`, preceded by `prefix`.
    pub fn write_prefixed(
        &self,
        formatter: &mut fmt::Formatter,
        prefix: &'static str,
    ) -> fmt::Result {
        formatter.write_str(prefix)?;
        <usize as fmt::Display>::fmt(&self.index(), formatter)
    }
}
