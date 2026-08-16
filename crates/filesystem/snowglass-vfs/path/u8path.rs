use super::{Component, Components, Path, PathOwned, PathStr, components::State};
use bstr::{BStr, BString, ByteSlice};
use std::{
    borrow::Borrow,
    fmt,
    hash::{Hash, Hasher},
    ops::Deref,
};

#[repr(transparent)]
#[derive(Clone, Default)]
pub struct U8PathBuf(BString);

#[repr(transparent)]
pub struct U8Path(BStr);

impl U8PathBuf {
    pub fn from_vec(bytes: Vec<u8>) -> Self {
        Self(BString::new(bytes))
    }
    pub fn into_vec(self) -> Vec<u8> {
        self.0.into()
    }
}
impl U8Path {
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}
impl From<&U8Path> for U8PathBuf {
    fn from(value: &U8Path) -> Self {
        Self(BString::new(value.0.to_vec()))
    }
}
impl From<&str> for U8PathBuf {
    fn from(value: &str) -> Self {
        Self(BString::from(value))
    }
}
impl Borrow<U8Path> for U8PathBuf {
    fn borrow(&self) -> &U8Path {
        U8Path::from_str(BStr::new(self.0.as_slice()))
    }
}
impl Deref for U8PathBuf {
    type Target = U8Path;
    fn deref(&self) -> &Self::Target {
        self.borrow()
    }
}
impl AsRef<U8Path> for U8PathBuf {
    fn as_ref(&self) -> &U8Path {
        self.borrow()
    }
}
impl ToOwned for U8Path {
    type Owned = U8PathBuf;
    fn to_owned(&self) -> Self::Owned {
        self.into()
    }
}
impl PartialEq for U8Path {
    fn eq(&self, other: &Self) -> bool {
        self.components() == other.components()
    }
}
impl Eq for U8Path {}
impl PartialEq for U8PathBuf {
    fn eq(&self, other: &Self) -> bool {
        self.as_ref() == other.as_ref()
    }
}
impl Eq for U8PathBuf {}
impl Hash for U8PathBuf {
    fn hash<H: Hasher>(&self, state: &mut H) {
        for component in self.components() {
            match component {
                Component::Root => 0_u8.hash(state),
                Component::Current => 1_u8.hash(state),
                Component::Parent => 2_u8.hash(state),
                Component::Normal(value) => {
                    3_u8.hash(state);
                    value.as_bytes().hash(state);
                }
            }
        }
    }
}
impl fmt::Debug for U8Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("U8Path").field(&&self.0).finish()
    }
}
impl fmt::Debug for U8PathBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_ref().fmt(f)
    }
}

impl Path for U8Path {
    type Str = BStr;
    const CURRENT_DIR: &'static BStr = bstr_literal(b".");
    const PARENT_DIR: &'static BStr = bstr_literal(b"..");
    const SEPARATOR: u8 = b'/';
    fn is_separator(unit: u8) -> bool {
        unit == b'/'
    }
    fn root() -> &'static Self {
        Self::from_str(BStr::new(b"/"))
    }
    fn empty() -> &'static Self {
        Self::from_str(BStr::new(b""))
    }
    fn has_root(&self) -> bool {
        self.0.first().is_some_and(|unit| Self::is_separator(*unit))
    }
    fn components(&self) -> Components<'_, Self> {
        Components {
            path: &self.0,
            has_root: self.has_root(),
            front: State::StartDir,
            back: State::Body,
        }
    }
    fn from_str(value: &Self::Str) -> &Self {
        // SAFETY: both types are transparent wrappers over `[u8]`.
        unsafe { &*(value as *const BStr as *const Self) }
    }
    fn as_str(&self) -> &Self::Str {
        &self.0
    }
}

impl PathOwned for U8PathBuf {
    type Borrowed = U8Path;
    fn new() -> Self {
        Self::default()
    }
    fn push(&mut self, component: &BStr) {
        if component.is_empty() {
            return;
        }
        if !self.0.is_empty() && !U8Path::is_separator(*self.0.last().unwrap()) {
            self.0.push(b'/');
        }
        self.0.extend_from_slice(component.as_bytes());
    }
    fn pop(&mut self) -> bool {
        let root = self.has_root();
        let mut components = self
            .components()
            .filter_map(|component| match component {
                Component::Normal(value) => Some(value.to_vec()),
                Component::Parent => Some(U8Path::PARENT_DIR.to_vec()),
                Component::Root | Component::Current => None,
            })
            .collect::<Vec<_>>();
        if components.pop().is_none() {
            return false;
        }
        self.0.clear();
        if root {
            self.0.push(b'/');
        }
        for component in components {
            self.push(BStr::new(&component));
        }
        true
    }
}

const fn bstr_literal(bytes: &[u8]) -> &BStr {
    // SAFETY: BStr is a transparent wrapper around [u8].
    unsafe { &*(bytes as *const [u8] as *const BStr) }
}
