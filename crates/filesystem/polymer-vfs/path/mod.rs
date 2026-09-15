//! Native-unit path handling adapted from AincradFS.
//!
//! Byte paths and UTF-16 paths are distinct types. Neither owned type uses a
//! Rust UTF-8 `String`, so platform adapters do not need a lossy intermediate
//! representation.

mod components;
mod u16path;
mod u8path;
mod vfs;

pub use components::{Component, Components};
pub use u8path::{U8Path, U8PathBuf};
pub use u16path::{U16Path, U16PathBuf};
pub use vfs::{
    NativePathSemantics, PathSemantics, PosixPathSemantics, VfsComponent, VfsPath, VfsPathBuf,
    VfsPathUnit, WindowsPathSemantics,
};

use bstr::{BStr, ByteSlice};
use std::{borrow::Borrow, hash::Hash};
use widestring::U16Str;

pub trait PathStr: 'static + Eq + Hash {
    type Unit: Copy + Eq + Hash;

    fn as_slice(&self) -> &[Self::Unit];
    fn from_slice(slice: &[Self::Unit]) -> &Self;

    fn len(&self) -> usize {
        self.as_slice().len()
    }

    fn is_empty(&self) -> bool {
        self.as_slice().is_empty()
    }
}

impl PathStr for U16Str {
    type Unit = u16;

    fn as_slice(&self) -> &[u16] {
        U16Str::as_slice(self)
    }

    fn from_slice(slice: &[u16]) -> &Self {
        U16Str::from_slice(slice)
    }
}

impl PathStr for BStr {
    type Unit = u8;

    fn as_slice(&self) -> &[u8] {
        self.as_bytes()
    }

    fn from_slice(slice: &[u8]) -> &Self {
        BStr::new(slice)
    }
}

pub trait Path: Eq {
    type Str: PathStr + ?Sized;

    const CURRENT_DIR: &'static Self::Str;
    const PARENT_DIR: &'static Self::Str;
    const SEPARATOR: <Self::Str as PathStr>::Unit;

    fn is_separator(unit: <Self::Str as PathStr>::Unit) -> bool;
    fn root() -> &'static Self;
    fn empty() -> &'static Self;
    fn has_root(&self) -> bool;
    fn components(&self) -> Components<'_, Self>;
    fn from_str(value: &Self::Str) -> &Self;
    fn as_str(&self) -> &Self::Str;
}

pub trait PathOwned: Clone + Eq + Borrow<Self::Borrowed> {
    type Borrowed: Path + ?Sized;

    fn new() -> Self;
    fn push(&mut self, component: &<Self::Borrowed as Path>::Str);
    fn pop(&mut self) -> bool;
}
