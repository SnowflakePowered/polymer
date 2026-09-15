use super::{Path, PathOwned, PathStr, U8Path, U8PathBuf, U16Path, U16PathBuf};
use crate::{VfsError, VfsResult};
use bstr::{BStr, BString, ByteSlice};
use std::{borrow::Borrow, fmt, hash::Hash, marker::PhantomData, str::FromStr};
use widestring::{U16Str, U16String};

pub trait PathSemantics:
    Clone + Copy + Default + fmt::Debug + Eq + Hash + Ord + Send + Sync + 'static
{
    type Unit: Copy + fmt::Debug + Eq + Hash + Ord + Send + Sync + 'static;
    type Str: PathStr<Unit = Self::Unit> + ?Sized;
    type String: Borrow<Self::Str> + Clone + fmt::Debug + Eq + Hash + Ord + Send + Sync + 'static;
    type Path: Path<Str = Self::Str> + ToOwned<Owned = Self::PathBuf> + ?Sized;
    type PathBuf: PathOwned<Borrowed = Self::Path>
        + Clone
        + fmt::Debug
        + Eq
        + Hash
        + Send
        + Sync
        + 'static;

    fn encode(value: &str) -> Vec<Self::Unit>;
    fn string_from_units(units: Vec<Self::Unit>) -> Self::String;
    fn path_buf_from_units(units: Vec<Self::Unit>) -> Self::PathBuf;
    fn units_to_string_lossy(units: &[Self::Unit]) -> String;
    fn is_nul(unit: Self::Unit) -> bool;

    fn is_reserved_name(_units: &[Self::Unit]) -> bool {
        false
    }

    fn validate_platform_component(_units: &[Self::Unit]) -> VfsResult<()> {
        Ok(())
    }

    fn normalize_valid_component(units: &[Self::Unit]) -> Vec<Self::Unit> {
        units.to_vec()
    }

    fn validate_component(units: &[Self::Unit]) -> VfsResult<()> {
        let current = <Self::Path as Path>::CURRENT_DIR.as_slice();
        let parent = <Self::Path as Path>::PARENT_DIR.as_slice();
        if units.is_empty() || units == current || units == parent {
            return Err(VfsError::invalid_path::<Self>(
                units,
                "empty, '.' and '..' components are forbidden",
            ));
        }
        if units
            .iter()
            .any(|unit| Self::is_nul(*unit) || <Self::Path as Path>::is_separator(*unit))
        {
            return Err(VfsError::invalid_path::<Self>(
                units,
                "component contains a separator or NUL",
            ));
        }
        Self::validate_platform_component(units)?;
        if Self::is_reserved_name(units) {
            return Err(VfsError::invalid_path::<Self>(
                units,
                "component is a reserved device name",
            ));
        }
        Ok(())
    }

    fn normalized_component(units: &[Self::Unit]) -> VfsResult<Vec<Self::Unit>> {
        Self::validate_component(units)?;
        Ok(Self::normalize_valid_component(units))
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WindowsPathSemantics;

impl PathSemantics for WindowsPathSemantics {
    type Unit = u16;
    type Str = U16Str;
    type String = U16String;
    type Path = U16Path;
    type PathBuf = U16PathBuf;

    fn encode(value: &str) -> Vec<Self::Unit> {
        value.encode_utf16().collect()
    }

    fn string_from_units(units: Vec<Self::Unit>) -> Self::String {
        U16String::from_vec(units)
    }

    fn path_buf_from_units(units: Vec<Self::Unit>) -> Self::PathBuf {
        U16PathBuf::from_vec(units)
    }

    fn units_to_string_lossy(units: &[Self::Unit]) -> String {
        U16String::from_vec(units.to_vec()).to_string_lossy()
    }

    fn is_nul(unit: Self::Unit) -> bool {
        unit == 0
    }

    fn is_reserved_name(units: &[Self::Unit]) -> bool {
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

    fn validate_platform_component(units: &[Self::Unit]) -> VfsResult<()> {
        if units
            .last()
            .is_some_and(|unit| matches!(*unit, 0x20 | 0x2e))
        {
            return Err(VfsError::invalid_path::<Self>(
                units,
                "Windows names cannot end in a space or dot",
            ));
        }
        if units.iter().any(|unit| {
            *unit < 0x20 || matches!(*unit, 0x22 | 0x2a | 0x3a | 0x3c | 0x3e | 0x3f | 0x7c)
        }) {
            return Err(VfsError::invalid_path::<Self>(
                units,
                "component is not a valid Windows name",
            ));
        }
        Ok(())
    }

    fn normalize_valid_component(units: &[Self::Unit]) -> Vec<Self::Unit> {
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

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PosixPathSemantics;

impl PathSemantics for PosixPathSemantics {
    type Unit = u8;
    type Str = BStr;
    type String = BString;
    type Path = U8Path;
    type PathBuf = U8PathBuf;

    fn encode(value: &str) -> Vec<Self::Unit> {
        value.as_bytes().to_vec()
    }

    fn string_from_units(units: Vec<Self::Unit>) -> Self::String {
        BString::new(units)
    }

    fn path_buf_from_units(units: Vec<Self::Unit>) -> Self::PathBuf {
        U8PathBuf::from_vec(units)
    }

    fn units_to_string_lossy(units: &[Self::Unit]) -> String {
        units.to_str_lossy().into_owned()
    }

    fn is_nul(unit: Self::Unit) -> bool {
        unit == 0
    }
}

#[cfg(windows)]
pub type NativePathSemantics = WindowsPathSemantics;
#[cfg(unix)]
pub type NativePathSemantics = PosixPathSemantics;
#[cfg(not(any(windows, unix)))]
compile_error!("polymer-vfs path handling requires a Windows or Unix target");

pub type VfsPathUnit = <NativePathSemantics as PathSemantics>::Unit;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct VfsComponent<S: PathSemantics = NativePathSemantics> {
    value: S::String,
    semantics: PhantomData<S>,
}

impl<S: PathSemantics> VfsComponent<S> {
    pub fn new(value: impl AsRef<str>) -> VfsResult<Self> {
        Self::from_units(S::encode(value.as_ref()))
    }

    pub fn from_units(units: Vec<S::Unit>) -> VfsResult<Self> {
        S::validate_component(&units)?;
        Ok(Self {
            value: S::string_from_units(units),
            semantics: PhantomData,
        })
    }

    pub fn as_str(&self) -> &S::Str {
        self.value.borrow()
    }

    pub fn as_slice(&self) -> &[S::Unit] {
        self.as_str().as_slice()
    }

    pub fn to_string_lossy(&self) -> String {
        S::units_to_string_lossy(self.as_slice())
    }

    pub(crate) fn normalized(&self) -> VfsResult<Vec<S::Unit>> {
        S::normalized_component(self.as_slice())
    }
}

impl<S: PathSemantics> fmt::Display for VfsComponent<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_string_lossy())
    }
}

#[repr(transparent)]
pub struct VfsPath<S: PathSemantics = NativePathSemantics>(S::Path);

impl<S: PathSemantics> VfsPath<S> {
    pub fn from_path(path: &S::Path) -> VfsResult<&Self> {
        Self::validate_path(path.as_str().as_slice())?;
        Ok(Self::from_path_unchecked(path))
    }

    fn validate_path(units: &[S::Unit]) -> VfsResult<()> {
        let separator = <S::Path as Path>::SEPARATOR;
        if units.first() != Some(&separator) {
            return Err(VfsError::invalid_path::<S>(units, "path must be absolute"));
        }
        if units.len() == 1 {
            return Ok(());
        }
        if units.len() > 1 && units.last() == Some(&separator) {
            return Err(VfsError::invalid_path::<S>(
                units,
                "trailing separators are not canonical",
            ));
        }
        for component in units[1..].split(|unit| *unit == separator) {
            S::validate_component(component)
                .map_err(|error| VfsError::invalid_path::<S>(units, error.to_string()))?;
        }
        Ok(())
    }

    fn from_path_unchecked(path: &S::Path) -> &Self {
        // SAFETY: VfsPath is transparent over the semantics' borrowed path.
        unsafe { &*(path as *const S::Path as *const Self) }
    }

    pub fn as_inner(&self) -> &S::Path {
        &self.0
    }

    pub fn as_slice(&self) -> &[S::Unit] {
        self.0.as_str().as_slice()
    }

    pub fn is_root(&self) -> bool {
        &self.0 == <S::Path as Path>::root()
    }

    pub fn components(&self) -> impl DoubleEndedIterator<Item = &S::Str> {
        self.0.components().filter_map(|component| match component {
            super::Component::Normal(value) => Some(value),
            super::Component::Root | super::Component::Current | super::Component::Parent => None,
        })
    }

    pub fn parent(&self) -> Option<VfsPathBuf<S>> {
        if self.is_root() {
            return None;
        }
        let mut owned = VfsPathBuf {
            value: self.0.to_owned(),
            semantics: PhantomData,
        };
        let popped = PathOwned::pop(&mut owned.value);
        debug_assert!(popped);
        let path: &S::Path = Borrow::borrow(&owned.value);
        if path.as_str().is_empty() {
            Some(VfsPathBuf::root())
        } else {
            Some(owned)
        }
    }

    pub fn join(&self, component: &VfsComponent<S>) -> VfsPathBuf<S> {
        let mut value = self.0.to_owned();
        PathOwned::push(&mut value, component.as_str());
        VfsPathBuf {
            value,
            semantics: PhantomData,
        }
    }

    pub fn to_string_lossy(&self) -> String {
        S::units_to_string_lossy(self.as_slice())
    }
}

impl<S: PathSemantics> PartialEq for VfsPath<S> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl<S: PathSemantics> Eq for VfsPath<S> {}

impl<S: PathSemantics> fmt::Debug for VfsPath<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("VfsPath")
            .field(&self.to_string_lossy())
            .finish()
    }
}

impl<S: PathSemantics> fmt::Display for VfsPath<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_string_lossy())
    }
}

impl<S: PathSemantics> ToOwned for VfsPath<S> {
    type Owned = VfsPathBuf<S>;

    fn to_owned(&self) -> Self::Owned {
        VfsPathBuf {
            value: self.0.to_owned(),
            semantics: PhantomData,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct VfsPathBuf<S: PathSemantics = NativePathSemantics> {
    value: S::PathBuf,
    semantics: PhantomData<S>,
}

impl<S: PathSemantics> VfsPathBuf<S> {
    pub fn root() -> Self {
        Self {
            value: <S::Path as Path>::root().to_owned(),
            semantics: PhantomData,
        }
    }

    pub fn new(value: impl AsRef<str>) -> VfsResult<Self> {
        Self::from_units(S::encode(value.as_ref()))
    }

    pub fn from_units(units: Vec<S::Unit>) -> VfsResult<Self> {
        VfsPath::<S>::validate_path(&units)?;
        Ok(Self {
            value: S::path_buf_from_units(units),
            semantics: PhantomData,
        })
    }

    pub fn as_path(&self) -> &VfsPath<S> {
        VfsPath::from_path_unchecked(self.value.borrow())
    }

    pub fn as_slice(&self) -> &[S::Unit] {
        self.as_path().as_slice()
    }

    pub fn components(&self) -> impl DoubleEndedIterator<Item = &S::Str> {
        self.as_path().components()
    }

    pub fn parent(&self) -> Option<Self> {
        self.as_path().parent()
    }

    pub fn normalized_components(&self) -> VfsResult<Vec<Vec<S::Unit>>> {
        self.components()
            .map(|component| S::normalized_component(component.as_slice()))
            .collect()
    }

    pub fn to_string_lossy(&self) -> String {
        self.as_path().to_string_lossy()
    }
}

#[cfg(windows)]
impl VfsPathBuf<WindowsPathSemantics> {
    pub fn to_host_path(&self) -> VfsResult<std::path::PathBuf> {
        use std::os::windows::ffi::OsStringExt;
        Ok(std::ffi::OsString::from_wide(self.as_slice()).into())
    }
}

#[cfg(unix)]
impl VfsPathBuf<PosixPathSemantics> {
    pub fn to_host_path(&self) -> VfsResult<std::path::PathBuf> {
        use std::os::unix::ffi::OsStringExt;
        Ok(std::ffi::OsString::from_vec(self.as_slice().to_vec()).into())
    }
}

impl<S: PathSemantics> AsRef<VfsPath<S>> for VfsPathBuf<S> {
    fn as_ref(&self) -> &VfsPath<S> {
        self.as_path()
    }
}

impl<S: PathSemantics> Borrow<VfsPath<S>> for VfsPathBuf<S> {
    fn borrow(&self) -> &VfsPath<S> {
        self.as_path()
    }
}

impl<S: PathSemantics> fmt::Display for VfsPathBuf<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_path().fmt(formatter)
    }
}

impl<S: PathSemantics> FromStr for VfsPathBuf<S> {
    type Err = VfsError;

    fn from_str(value: &str) -> VfsResult<Self> {
        Self::new(value)
    }
}
