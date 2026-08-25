mod handle;
mod non_max_u32;

pub use handle::*;

use core::{fmt, ops, slice};

use crate::error::{Error, Result};

/// An arena holding values that can be referenced by strongly typed handles.
#[derive(Clone)]
pub struct Arena<T> {
    data: Vec<T>,
}

impl<T> Default for Arena<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: fmt::Debug> fmt::Debug for Arena<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl<T> Arena<T> {
    /// Creates an empty arena.
    pub const fn new() -> Self {
        Self { data: Vec::new() }
    }

    /// Extracts the values from the arena.
    pub fn into_inner(self) -> Vec<T> {
        self.data
    }

    /// Returns the number of values in the arena.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Returns whether the arena contains no values.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Returns the arena's values without their handles.
    pub fn values(&self) -> slice::Iter<'_, T> {
        self.data.iter()
    }

    /// Returns the arena's values mutably, without their handles.
    pub fn values_mut(&mut self) -> slice::IterMut<'_, T> {
        self.data.iter_mut()
    }

    /// Iterates over each value and its handle.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (Handle<T>, &T)> + ExactSizeIterator {
        self.data
            .iter()
            .enumerate()
            .map(|(index, value)| (Handle::from_usize(index), value))
    }

    /// Mutably iterates over each value and its handle.
    pub fn iter_mut(
        &mut self,
    ) -> impl DoubleEndedIterator<Item = (Handle<T>, &mut T)> + ExactSizeIterator {
        self.data
            .iter_mut()
            .enumerate()
            .map(|(index, value)| (Handle::from_usize(index), value))
    }

    /// Adds a value and returns its handle.
    pub fn append(&mut self, value: T) -> Handle<T> {
        let handle = Handle::from_usize(self.data.len());
        self.data.push(value);
        handle
    }

    /// Finds the first value matching `predicate`.
    pub fn fetch_if(&self, predicate: impl Fn(&T) -> bool) -> Option<Handle<T>> {
        self.data.iter().position(predicate).map(Handle::from_usize)
    }

    /// Finds an equivalent value or appends `value` when none exists.
    pub fn fetch_if_or_append(
        &mut self,
        value: T,
        equivalent: impl Fn(&T, &T) -> bool,
    ) -> Handle<T> {
        self.data
            .iter()
            .position(|existing| equivalent(existing, &value))
            .map(Handle::from_usize)
            .unwrap_or_else(|| self.append(value))
    }

    /// Finds an equal value or appends `value` when none exists.
    pub fn fetch_or_append(&mut self, value: T) -> Handle<T>
    where
        T: PartialEq,
    {
        self.fetch_if_or_append(value, T::eq)
    }

    /// Returns a value when `handle` belongs to this arena.
    pub fn try_get(&self, handle: Handle<T>) -> Result<&T> {
        self.data
            .get(handle.index())
            .ok_or_else(|| Error::bad_handle::<T, ()>(handle.index()))
    }

    /// Returns a mutable reference to the value at `handle`.
    pub fn get_mut(&mut self, handle: Handle<T>) -> &mut T {
        &mut self.data[handle.index()]
    }

    /// Removes all values while retaining the allocation.
    pub fn clear(&mut self) {
        self.data.clear();
    }

    /// Checks that `handle` belongs to this arena.
    pub fn check_contains_handle(&self, handle: Handle<T>) -> Result<()> {
        if handle.index() < self.data.len() {
            Ok(())
        } else {
            Err(Error::bad_handle::<T, ()>(handle.index()))
        }
    }

    pub(crate) fn retain_mut(&mut self, mut predicate: impl FnMut(Handle<T>, &mut T) -> bool) {
        let mut index = 0;
        self.data.retain_mut(|value| {
            let keep = predicate(Handle::from_usize(index), value);
            index += 1;
            keep
        });
    }
}

impl<'a, T> IntoIterator for &'a Arena<T> {
    type Item = &'a T;
    type IntoIter = slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.values()
    }
}

impl<'a, T> IntoIterator for &'a mut Arena<T> {
    type Item = &'a mut T;
    type IntoIter = slice::IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.values_mut()
    }
}

impl<T> ops::Index<Handle<T>> for Arena<T> {
    type Output = T;

    fn index(&self, handle: Handle<T>) -> &Self::Output {
        &self.data[handle.index()]
    }
}

impl<T> ops::IndexMut<Handle<T>> for Arena<T> {
    fn index_mut(&mut self, handle: Handle<T>) -> &mut Self::Output {
        &mut self.data[handle.index()]
    }
}
