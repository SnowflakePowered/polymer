use super::{Component, Components, Path, PathOwned, components::State};
use std::{
    borrow::Borrow,
    fmt,
    hash::{Hash, Hasher},
    ops::Deref,
};
use widestring::{U16Str, U16String, u16str};

#[repr(transparent)]
#[derive(Clone, Default)]
pub struct U16PathBuf(U16String);

#[repr(transparent)]
pub struct U16Path(U16Str);

impl U16PathBuf {
    pub fn from_vec(units: Vec<u16>) -> Self {
        Self(U16String::from_vec(units))
    }

    pub fn from_utf8(value: &str) -> Self {
        Self(U16String::from_str(value))
    }

    pub fn into_vec(self) -> Vec<u16> {
        self.0.into_vec()
    }
}

impl U16Path {
    pub fn as_slice(&self) -> &[u16] {
        self.0.as_slice()
    }

    pub fn to_string_lossy(&self) -> String {
        self.0.to_string_lossy()
    }
}

impl From<&U16Path> for U16PathBuf {
    fn from(value: &U16Path) -> Self {
        Self(value.0.to_ustring())
    }
}

impl From<&str> for U16PathBuf {
    fn from(value: &str) -> Self {
        Self::from_utf8(value)
    }
}

impl Borrow<U16Path> for U16PathBuf {
    fn borrow(&self) -> &U16Path {
        U16Path::from_str(&self.0)
    }
}

impl Deref for U16PathBuf {
    type Target = U16Path;
    fn deref(&self) -> &Self::Target {
        self.borrow()
    }
}

impl AsRef<U16Path> for U16PathBuf {
    fn as_ref(&self) -> &U16Path {
        self.borrow()
    }
}

impl ToOwned for U16Path {
    type Owned = U16PathBuf;
    fn to_owned(&self) -> Self::Owned {
        self.into()
    }
}

impl PartialEq for U16Path {
    fn eq(&self, other: &Self) -> bool {
        self.components() == other.components()
    }
}
impl Eq for U16Path {}
impl PartialEq for U16PathBuf {
    fn eq(&self, other: &Self) -> bool {
        self.as_ref() == other.as_ref()
    }
}
impl Eq for U16PathBuf {}
impl Hash for U16PathBuf {
    fn hash<H: Hasher>(&self, state: &mut H) {
        for component in self.components() {
            match component {
                Component::Root => 0_u8.hash(state),
                Component::Current => 1_u8.hash(state),
                Component::Parent => 2_u8.hash(state),
                Component::Normal(value) => {
                    3_u8.hash(state);
                    value.as_slice().hash(state);
                }
            }
        }
    }
}

impl fmt::Debug for U16Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("U16Path")
            .field(&self.to_string_lossy())
            .finish()
    }
}
impl fmt::Debug for U16PathBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_ref().fmt(f)
    }
}

impl Path for U16Path {
    type Str = U16Str;
    const CURRENT_DIR: &'static U16Str = u16str!(".");
    const PARENT_DIR: &'static U16Str = u16str!("..");
    const SEPARATOR: u16 = b'/' as u16;

    fn is_separator(unit: u16) -> bool {
        matches!(unit, 0x2f | 0x5c)
    }
    fn root() -> &'static Self {
        Self::from_str(u16str!("/"))
    }
    fn has_root(&self) -> bool {
        self.0
            .as_slice()
            .first()
            .is_some_and(|unit| Self::is_separator(*unit))
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
        // SAFETY: both types are transparent wrappers over `[u16]`.
        unsafe { &*(value as *const U16Str as *const Self) }
    }
    fn as_str(&self) -> &Self::Str {
        &self.0
    }
}

impl PathOwned for U16PathBuf {
    type Borrowed = U16Path;
    fn push(&mut self, component: &U16Str) {
        if component.is_empty() {
            return;
        }
        if !self.0.is_empty() && !U16Path::is_separator(*self.0.as_slice().last().unwrap()) {
            self.0.push_char('/');
        }
        self.0.push(component);
    }
    fn pop(&mut self) -> bool {
        let root = self.has_root();
        let mut components = self
            .components()
            .filter_map(|component| match component {
                Component::Normal(value) => Some(value.to_ustring()),
                Component::Parent => Some(U16Path::PARENT_DIR.to_ustring()),
                Component::Root | Component::Current => None,
            })
            .collect::<Vec<_>>();
        if components.pop().is_none() {
            return false;
        }
        self.0.clear();
        if root {
            self.0.push_char('/');
        }
        for component in components {
            self.push(&component);
        }
        true
    }
}
