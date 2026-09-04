use super::{Path, PathStr};
use std::{hash::Hash, iter::FusedIterator};

#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd)]
pub(crate) enum State {
    StartDir,
    Body,
    Done,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Component<'a, P: Path + ?Sized> {
    Root,
    Current,
    Parent,
    Normal(&'a P::Str),
}

#[must_use = "iterators are lazy and do nothing unless consumed"]
pub struct Components<'a, P: Path + ?Sized> {
    pub(crate) path: &'a P::Str,
    pub(crate) has_root: bool,
    pub(crate) front: State,
    pub(crate) back: State,
}

impl<P: Path + ?Sized> Clone for Components<'_, P> {
    fn clone(&self) -> Self {
        Self {
            path: self.path,
            has_root: self.has_root,
            front: self.front,
            back: self.back,
        }
    }
}

impl<'a, P: Path + ?Sized> Components<'a, P> {
    fn parse_single(&self, component: &'a P::Str) -> Option<Component<'a, P>> {
        if component == P::CURRENT_DIR || component.is_empty() {
            None
        } else if component == P::PARENT_DIR {
            Some(Component::Parent)
        } else {
            Some(Component::Normal(component))
        }
    }

    fn finished(&self) -> bool {
        self.front == State::Done || self.back == State::Done || self.front > self.back
    }

    fn include_current(&self) -> bool {
        if self.has_root {
            return false;
        }
        let current = P::CURRENT_DIR.as_slice();
        let path = self.path.as_slice();
        path.starts_with(current)
            && (path.len() == current.len()
                || path
                    .get(current.len())
                    .is_some_and(|unit| P::is_separator(*unit)))
    }

    fn len_before_body(&self) -> usize {
        usize::from(self.front <= State::StartDir && (self.has_root || self.include_current()))
    }

    fn parse_front(&self) -> (usize, Option<Component<'a, P>>) {
        let slice = self.path.as_slice();
        match slice.iter().position(|unit| P::is_separator(*unit)) {
            Some(index) => (
                index + 1,
                self.parse_single(P::Str::from_slice(&slice[..index])),
            ),
            None => (slice.len(), self.parse_single(self.path)),
        }
    }

    fn parse_back(&self) -> (usize, Option<Component<'a, P>>) {
        let slice = self.path.as_slice();
        let start = self.len_before_body().min(slice.len());
        match slice[start..]
            .iter()
            .rposition(|unit| P::is_separator(*unit))
        {
            Some(index) => {
                let component = &slice[start + index + 1..];
                (
                    component.len() + 1,
                    self.parse_single(P::Str::from_slice(component)),
                )
            }
            None => {
                let component = &slice[start..];
                (
                    component.len(),
                    self.parse_single(P::Str::from_slice(component)),
                )
            }
        }
    }

    pub fn as_path(&self) -> &'a P {
        P::from_str(self.path)
    }
}

impl<'a, P: Path + ?Sized> Iterator for Components<'a, P> {
    type Item = Component<'a, P>;

    fn next(&mut self) -> Option<Self::Item> {
        while !self.finished() {
            match self.front {
                State::StartDir => {
                    self.front = State::Body;
                    if self.has_root {
                        self.path = P::Str::from_slice(&self.path.as_slice()[1..]);
                        return Some(Component::Root);
                    }
                    if self.include_current() {
                        self.path = P::Str::from_slice(&self.path.as_slice()[1..]);
                        return Some(Component::Current);
                    }
                }
                State::Body if !self.path.is_empty() => {
                    let (size, component) = self.parse_front();
                    self.path = P::Str::from_slice(&self.path.as_slice()[size..]);
                    if component.is_some() {
                        return component;
                    }
                }
                State::Body => self.front = State::Done,
                State::Done => unreachable!(),
            }
        }
        None
    }
}

impl<'a, P: Path + ?Sized> DoubleEndedIterator for Components<'a, P> {
    fn next_back(&mut self) -> Option<Self::Item> {
        while !self.finished() {
            match self.back {
                State::Body if self.path.len() > self.len_before_body() => {
                    let (size, component) = self.parse_back();
                    let remaining = self.path.len() - size;
                    self.path = P::Str::from_slice(&self.path.as_slice()[..remaining]);
                    if component.is_some() {
                        return component;
                    }
                }
                State::Body => self.back = State::StartDir,
                State::StartDir => {
                    self.back = State::Done;
                    if self.has_root {
                        return Some(Component::Root);
                    }
                    if self.include_current() {
                        return Some(Component::Current);
                    }
                }
                State::Done => unreachable!(),
            }
        }
        None
    }
}

impl<P: Path + ?Sized> FusedIterator for Components<'_, P> {}

impl<P: Path + ?Sized> PartialEq for Components<'_, P> {
    fn eq(&self, other: &Self) -> bool {
        Iterator::eq(self.clone(), other.clone())
    }
}

impl<P: Path + ?Sized> Eq for Components<'_, P> {}
