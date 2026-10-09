#[cfg(unix)]
use super::PathStr;
use super::{Path, PathOwned};
#[cfg(unix)]
use super::{U8Path, U8PathBuf};
#[cfg(windows)]
use super::{U16Path, U16PathBuf};
use crate::{VfsError, VfsResult};
#[cfg(unix)]
use bstr::{BStr, BString, ByteSlice};
use std::{borrow::Borrow, fmt, str::FromStr};
#[cfg(windows)]
use widestring::{U16Str, U16String};

#[cfg(windows)]
pub type VfsPathUnit = u16;
#[cfg(windows)]
pub type VfsStr = U16Str;
#[cfg(windows)]
type NativeString = U16String;
#[cfg(windows)]
type NativePath = U16Path;
#[cfg(windows)]
type NativePathBuf = U16PathBuf;

#[cfg(unix)]
pub type VfsPathUnit = u8;
#[cfg(unix)]
pub type VfsStr = BStr;
#[cfg(unix)]
type NativeString = BString;
#[cfg(unix)]
type NativePath = U8Path;
#[cfg(unix)]
type NativePathBuf = U8PathBuf;

#[cfg(not(any(windows, unix)))]
compile_error!("polymer-vfs path handling requires a Windows or Unix target");

pub(crate) struct Native;

#[cfg(windows)]
impl Native {
    fn encode(value: &str) -> Vec<VfsPathUnit> {
        value.encode_utf16().collect()
    }

    fn string_from_units(units: Vec<VfsPathUnit>) -> NativeString {
        U16String::from_vec(units)
    }

    fn path_buf_from_units(units: Vec<VfsPathUnit>) -> NativePathBuf {
        U16PathBuf::from_vec(units)
    }

    pub(crate) fn units_to_string_lossy(units: &[VfsPathUnit]) -> String {
        U16String::from_vec(units.to_vec()).to_string_lossy()
    }

    fn is_reserved_name(units: &[VfsPathUnit]) -> bool {
        let stem = units
            .split(|unit| *unit == b'.' as u16)
            .next()
            .unwrap_or(units);
        let ascii_upper = stem
            .iter()
            .map(|unit| {
                u8::try_from(*unit)
                    .ok()
                    .map(|byte| byte.to_ascii_uppercase())
            })
            .collect::<Option<Vec<_>>>();
        ascii_upper.as_deref().is_some_and(|value| {
            matches!(value, b"CON" | b"PRN" | b"AUX" | b"NUL")
                || value
                    .strip_prefix(b"COM")
                    .or_else(|| value.strip_prefix(b"LPT"))
                    .is_some_and(|number| {
                        matches!(
                            number,
                            b"1" | b"2" | b"3" | b"4" | b"5" | b"6" | b"7" | b"8" | b"9"
                        )
                    })
        })
    }

    fn validate_platform_component(units: &[VfsPathUnit]) -> VfsResult<()> {
        if units
            .last()
            .is_some_and(|unit| matches!(*unit, 0x20 | 0x2e))
        {
            return Err(VfsError::invalid_path(
                units,
                "Windows names cannot end in a space or dot",
            ));
        }
        if units.iter().any(|unit| {
            *unit < 0x20 || matches!(*unit, 0x22 | 0x2a | 0x3a | 0x3c | 0x3e | 0x3f | 0x7c)
        }) {
            return Err(VfsError::invalid_path(
                units,
                "component is not a valid Windows name",
            ));
        }
        Ok(())
    }

    fn normalize_valid_component(units: &[VfsPathUnit]) -> Vec<VfsPathUnit> {
        String::from_utf16(units).map_or_else(
            |_| {
                units
                    .iter()
                    .map(|unit| {
                        u8::try_from(*unit)
                            .ok()
                            .map_or(*unit, |byte| byte.to_ascii_lowercase() as u16)
                    })
                    .collect()
            },
            |value| value.to_lowercase().encode_utf16().collect(),
        )
    }
}

#[cfg(unix)]
impl Native {
    fn encode(value: &str) -> Vec<VfsPathUnit> {
        value.as_bytes().to_vec()
    }

    fn string_from_units(units: Vec<VfsPathUnit>) -> NativeString {
        BString::new(units)
    }

    fn path_buf_from_units(units: Vec<VfsPathUnit>) -> NativePathBuf {
        U8PathBuf::from_vec(units)
    }

    pub(crate) fn units_to_string_lossy(units: &[VfsPathUnit]) -> String {
        units.to_str_lossy().into_owned()
    }

    fn is_reserved_name(_units: &[VfsPathUnit]) -> bool {
        false
    }

    fn validate_platform_component(_units: &[VfsPathUnit]) -> VfsResult<()> {
        Ok(())
    }

    fn normalize_valid_component(units: &[VfsPathUnit]) -> Vec<VfsPathUnit> {
        units.to_vec()
    }
}

impl Native {
    fn validate_component(units: &[VfsPathUnit]) -> VfsResult<()> {
        let current = NativePath::CURRENT_DIR.as_slice();
        let parent = NativePath::PARENT_DIR.as_slice();
        if units.is_empty() || units == current || units == parent {
            return Err(VfsError::invalid_path(
                units,
                "empty, '.' and '..' components are forbidden",
            ));
        }
        if units
            .iter()
            .any(|unit| *unit == 0 || NativePath::is_separator(*unit))
        {
            return Err(VfsError::invalid_path(
                units,
                "component contains a separator or NUL",
            ));
        }
        Self::validate_platform_component(units)?;
        if Self::is_reserved_name(units) {
            return Err(VfsError::invalid_path(
                units,
                "component is a reserved device name",
            ));
        }
        Ok(())
    }

    fn normalized_component(units: &[VfsPathUnit]) -> VfsResult<Vec<VfsPathUnit>> {
        Self::validate_component(units)?;
        Ok(Self::normalize_valid_component(units))
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct VfsComponent {
    value: NativeString,
}

impl VfsComponent {
    pub fn new(value: impl AsRef<str>) -> VfsResult<Self> {
        Self::from_units(Native::encode(value.as_ref()))
    }

    pub fn from_units(units: Vec<VfsPathUnit>) -> VfsResult<Self> {
        Native::validate_component(&units)?;
        Ok(Self {
            value: Native::string_from_units(units),
        })
    }

    pub fn as_str(&self) -> &VfsStr {
        self.value.borrow()
    }

    pub fn as_slice(&self) -> &[VfsPathUnit] {
        self.as_str().as_slice()
    }

    pub fn to_string_lossy(&self) -> String {
        Native::units_to_string_lossy(self.as_slice())
    }

    pub(crate) fn normalized(&self) -> VfsResult<Vec<VfsPathUnit>> {
        Native::normalized_component(self.as_slice())
    }
}

impl fmt::Display for VfsComponent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_string_lossy())
    }
}

#[repr(transparent)]
pub struct VfsPath(NativePath);

impl VfsPath {
    pub fn from_path(path: &NativePath) -> VfsResult<&Self> {
        Self::validate_path(path.as_str().as_slice())?;
        Ok(Self::from_path_unchecked(path))
    }

    fn validate_path(units: &[VfsPathUnit]) -> VfsResult<()> {
        let separator = NativePath::SEPARATOR;
        if units.first() != Some(&separator) {
            return Err(VfsError::invalid_path(units, "path must be absolute"));
        }
        if units.len() == 1 {
            return Ok(());
        }
        if units.len() > 1 && units.last() == Some(&separator) {
            return Err(VfsError::invalid_path(
                units,
                "trailing separators are not canonical",
            ));
        }
        for component in units[1..].split(|unit| *unit == separator) {
            Native::validate_component(component)
                .map_err(|error| VfsError::invalid_path(units, error.to_string()))?;
        }
        Ok(())
    }

    fn from_path_unchecked(path: &NativePath) -> &Self {
        // SAFETY: VfsPath is transparent over the native borrowed path.
        unsafe { &*(path as *const NativePath as *const Self) }
    }

    pub fn as_slice(&self) -> &[VfsPathUnit] {
        self.0.as_str().as_slice()
    }

    pub fn is_root(&self) -> bool {
        &self.0 == NativePath::root()
    }

    pub fn components(&self) -> impl DoubleEndedIterator<Item = &VfsStr> {
        self.0.components().filter_map(|component| match component {
            super::Component::Normal(value) => Some(value),
            super::Component::Root | super::Component::Current | super::Component::Parent => None,
        })
    }

    pub fn parent(&self) -> Option<VfsPathBuf> {
        if self.is_root() {
            return None;
        }
        let mut owned = VfsPathBuf {
            value: self.0.to_owned(),
        };
        let popped = PathOwned::pop(&mut owned.value);
        debug_assert!(popped);
        let path: &NativePath = Borrow::borrow(&owned.value);
        if path.as_str().is_empty() {
            Some(VfsPathBuf::root())
        } else {
            Some(owned)
        }
    }

    pub fn join(&self, component: &VfsComponent) -> VfsPathBuf {
        let mut value = self.0.to_owned();
        PathOwned::push(&mut value, component.as_str());
        VfsPathBuf { value }
    }

    pub fn to_string_lossy(&self) -> String {
        Native::units_to_string_lossy(self.as_slice())
    }
}

impl PartialEq for VfsPath {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for VfsPath {}

impl fmt::Debug for VfsPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("VfsPath")
            .field(&self.to_string_lossy())
            .finish()
    }
}

impl fmt::Display for VfsPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_string_lossy())
    }
}

impl ToOwned for VfsPath {
    type Owned = VfsPathBuf;

    fn to_owned(&self) -> Self::Owned {
        VfsPathBuf {
            value: self.0.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct VfsPathBuf {
    value: NativePathBuf,
}

impl VfsPathBuf {
    pub fn root() -> Self {
        Self {
            value: NativePath::root().to_owned(),
        }
    }

    pub fn new(value: impl AsRef<str>) -> VfsResult<Self> {
        Self::from_units(Native::encode(value.as_ref()))
    }

    pub(crate) fn storage(path: &str) -> Self {
        Self::new(format!("/{path}")).unwrap_or_else(|_| Self::root())
    }

    pub fn from_units(units: Vec<VfsPathUnit>) -> VfsResult<Self> {
        VfsPath::validate_path(&units)?;
        Ok(Self {
            value: Native::path_buf_from_units(units),
        })
    }

    pub fn as_path(&self) -> &VfsPath {
        VfsPath::from_path_unchecked(self.value.borrow())
    }

    pub fn as_slice(&self) -> &[VfsPathUnit] {
        self.as_path().as_slice()
    }

    pub fn components(&self) -> impl DoubleEndedIterator<Item = &VfsStr> {
        self.as_path().components()
    }

    pub fn parent(&self) -> Option<Self> {
        self.as_path().parent()
    }

    pub fn normalized_components(&self) -> VfsResult<Vec<Vec<VfsPathUnit>>> {
        self.components()
            .map(|component| Native::normalized_component(component.as_slice()))
            .collect()
    }

    pub fn to_string_lossy(&self) -> String {
        self.as_path().to_string_lossy()
    }
}

impl AsRef<VfsPath> for VfsPathBuf {
    fn as_ref(&self) -> &VfsPath {
        self.as_path()
    }
}

impl Borrow<VfsPath> for VfsPathBuf {
    fn borrow(&self) -> &VfsPath {
        self.as_path()
    }
}

impl fmt::Display for VfsPathBuf {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_path().fmt(formatter)
    }
}

impl FromStr for VfsPathBuf {
    type Err = VfsError;

    fn from_str(value: &str) -> VfsResult<Self> {
        Self::new(value)
    }
}
