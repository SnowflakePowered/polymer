use std::{borrow::Borrow, cmp::Ordering, fmt, hash, iter, marker::PhantomData, sync::Arc};

use crate::error::{Error, Result};

/// Marker types distinguishing configuration path namespaces.
pub mod family {
    pub enum Generic {}
    pub enum Target {}
    pub enum Section {}
    pub enum Setting {}
}

/// A validated, period-separated configuration path.
pub struct ConfigurationPath<S = Arc<str>, F = family::Generic> {
    path: S,
    periods: Box<[usize]>,
    family: PhantomData<fn() -> F>,
}

/// A target path borrowed from a parsed TOML document.
pub type TargetPath<'de> = ConfigurationPath<&'de str, family::Target>;
/// An owned path identifying a target in a lowered definition.
pub type OwnedTargetPath = ConfigurationPath<Arc<str>, family::Target>;
/// An owned path identifying a presentation section.
pub type SectionPath = ConfigurationPath<Arc<str>, family::Section>;
/// An owned path identifying a concrete setting.
pub type SettingPath = ConfigurationPath<Arc<str>, family::Setting>;

impl<S: Clone, F> Clone for ConfigurationPath<S, F> {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            periods: self.periods.clone(),
            family: PhantomData,
        }
    }
}

impl<S: AsRef<str>, F> ConfigurationPath<S, F> {
    pub(crate) fn from_key(path: S) -> Self {
        let periods = path
            .as_ref()
            .match_indices('.')
            .map(|(index, _)| index)
            .collect();
        Self {
            path,
            periods,
            family: PhantomData,
        }
    }

    pub(crate) fn is_valid(path: &str) -> bool {
        !path.is_empty() && !path.starts_with('.') && !path.ends_with('.') && !path.contains("..")
    }

    /// The full path as a string.
    pub fn as_str(&self) -> &str {
        self.path.as_ref()
    }

    /// Iterates through the path components.
    pub fn components(&self) -> impl Iterator<Item = &str> + '_ {
        self.periods
            .iter()
            .copied()
            .chain(iter::once(self.as_str().len()))
            .scan(0, |start, end| {
                let component = &self.as_str()[*start..end];
                *start = end + 1;
                Some(component)
            })
    }

    /// Whether `self` is a strict parent of `other`.
    pub fn is_parent_of<T: AsRef<str>>(&self, other: &ConfigurationPath<T, F>) -> bool {
        self.periods.len() < other.periods.len()
            && other.as_str().starts_with(self.as_str())
            && other.as_str().as_bytes().get(self.as_str().len()) == Some(&b'.')
    }

    /// Whether `self` is a strict child of `other`.
    pub fn is_child_of<T: AsRef<str>>(&self, other: &ConfigurationPath<T, F>) -> bool {
        other.is_parent_of(self)
    }

    /// Copies this path into the default owned storage.
    pub fn to_owned_path(&self) -> ConfigurationPath<Arc<str>, F> {
        ConfigurationPath::from_key(Arc::from(self.as_str()))
    }
}

impl<F> ConfigurationPath<Arc<str>, F> {
    /// Creates an owned configuration path.
    pub fn new(path: impl Into<Arc<str>>) -> Result<Self> {
        let path = path.into();
        if Self::is_valid(&path) {
            Ok(Self::from_key(path))
        } else {
            Err(Error::InvalidConfigurationPath)
        }
    }

    pub(crate) fn join<G>(&self, component: &str) -> ConfigurationPath<Arc<str>, G> {
        ConfigurationPath::from_key(Arc::from(format!("{}.{component}", self.as_str())))
    }
}

impl<S: AsRef<str>, T: AsRef<str>, F> PartialEq<ConfigurationPath<T, F>>
    for ConfigurationPath<S, F>
{
    fn eq(&self, other: &ConfigurationPath<T, F>) -> bool {
        self.as_str() == other.as_str()
    }
}

impl<S: AsRef<str>, F> Eq for ConfigurationPath<S, F> {}

impl<S: AsRef<str>, T: AsRef<str>, F> PartialOrd<ConfigurationPath<T, F>>
    for ConfigurationPath<S, F>
{
    fn partial_cmp(&self, other: &ConfigurationPath<T, F>) -> Option<Ordering> {
        if self == other {
            Some(Ordering::Equal)
        } else if self.is_parent_of(other) {
            Some(Ordering::Less)
        } else if self.is_child_of(other) {
            Some(Ordering::Greater)
        } else {
            None
        }
    }
}

impl<S: AsRef<str>, F> hash::Hash for ConfigurationPath<S, F> {
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

impl<S: AsRef<str>, F> Borrow<str> for ConfigurationPath<S, F> {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl<S: AsRef<str>, F> AsRef<str> for ConfigurationPath<S, F> {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl<S: AsRef<str>, F> fmt::Debug for ConfigurationPath<S, F> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_str().fmt(formatter)
    }
}

impl<S: AsRef<str>, F> fmt::Display for ConfigurationPath<S, F> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}
