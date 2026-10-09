mod raw;

use std::borrow::{Borrow, Cow};
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::{Error, Storage, VersionedStorage};

pub(crate) use raw::MemoryCardStorage;

#[derive(Debug)]
pub enum CardBytes<'a> {
    Borrowed(&'a [u8]),
    Owned(Vec<u8>),
}

impl CardBytes<'_> {
    pub fn as_slice(&self) -> &[u8] {
        match self {
            Self::Borrowed(bytes) => bytes,
            Self::Owned(bytes) => bytes,
        }
    }

    pub fn as_mut_slice(&mut self) -> Option<&mut [u8]> {
        match self {
            Self::Borrowed(_) => None,
            Self::Owned(bytes) => Some(bytes),
        }
    }

    pub fn into_owned(self) -> Vec<u8> {
        match self {
            Self::Borrowed(bytes) => bytes.to_vec(),
            Self::Owned(bytes) => bytes,
        }
    }
}

pub(crate) struct ClaimCounts<K>(HashMap<K, usize>);

impl<K: Eq + Hash> FromIterator<K> for ClaimCounts<K> {
    fn from_iter<I: IntoIterator<Item = K>>(keys: I) -> Self {
        let mut counts = HashMap::new();
        for key in keys {
            *counts.entry(key).or_default() += 1;
        }
        Self(counts)
    }
}

impl<K: Eq + Hash> ClaimCounts<K> {
    pub(crate) fn count<Q: Eq + Hash + ?Sized>(&self, key: &Q) -> usize
    where
        K: Borrow<Q>,
    {
        self.0.get(key).copied().unwrap_or_default()
    }

    pub(crate) fn unique<Q: Eq + Hash + ?Sized>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
    {
        self.count(key) == 1
    }
}

pub(crate) struct WriteBackEntry<'a, I> {
    pub(crate) identity: I,
    pub(crate) paths: Vec<String>,
    pub(crate) contents: Option<Vec<Cow<'a, [u8]>>>,
}

struct Ownership<I> {
    owned: HashSet<String>,
    known: HashMap<I, Vec<String>>,
}

pub(crate) struct WriteBack<I> {
    storage: Arc<VersionedStorage>,
    ownership: Mutex<Ownership<I>>,
}

impl<I: Eq + Hash + Clone> WriteBack<I> {
    pub(crate) fn new(
        storage: Arc<VersionedStorage>,
        projected: impl IntoIterator<Item = (I, Vec<String>)>,
    ) -> Self {
        let known = projected.into_iter().collect::<HashMap<_, _>>();
        let owned = known.values().flatten().cloned().collect();
        Self {
            storage,
            ownership: Mutex::new(Ownership { owned, known }),
        }
    }

    pub(crate) fn pass(&self, entries: &[WriteBackEntry<'_, I>]) -> Result<(), Error> {
        let _pass = self.storage.hold_writes();
        let mut ownership = self.ownership.lock();
        let claims = entries
            .iter()
            .map(|entry| {
                entry
                    .paths
                    .iter()
                    .chain(ownership.known.get(&entry.identity).into_iter().flatten())
                    .map(String::as_str)
                    .collect::<HashSet<_>>()
            })
            .collect::<Vec<_>>();
        let counts = claims.iter().flatten().copied().collect::<ClaimCounts<_>>();
        let removed = ownership
            .owned
            .iter()
            .filter(|path| counts.count(path.as_str()) == 0)
            .cloned()
            .collect::<Vec<_>>();
        let written = entries
            .iter()
            .filter(|entry| {
                entry.contents.is_some()
                    && !entry.paths.is_empty()
                    && entry.paths.iter().all(|path| counts.unique(path.as_str()))
            })
            .collect::<Vec<_>>();
        for entry in &written {
            for (path, bytes) in entry.paths.iter().zip(entry.contents.iter().flatten()) {
                self.storage.write_if_changed(path, bytes)?;
            }
        }
        for path in &removed {
            self.storage.remove(path)?;
        }
        let Ownership { owned, known } = &mut *ownership;
        for path in &removed {
            owned.remove(path);
        }
        let present = entries
            .iter()
            .map(|entry| &entry.identity)
            .collect::<HashSet<_>>();
        known.retain(|identity, _| present.contains(identity));
        for entry in written {
            owned.extend(entry.paths.iter().cloned());
            known.insert(entry.identity.clone(), entry.paths.clone());
        }
        Ok(())
    }
}

pub trait RawMemoryCardImage<'a>: Sized {
    type Error;

    type Identity;

    type Save<'card>;

    const BLOCK_SIZE: u64;

    const MAX_BLOCKS: u32;

    /// Create an empty, owned memory-card image.
    ///
    /// `blocks` must be at most [`Self::MAX_BLOCKS`].
    fn empty(blocks: u32) -> Result<Self, Self::Error>;

    /// Create an immutable memory-card view over `bytes`.
    ///
    /// [`Self::BLOCK_SIZE`] * `blocks` must fit into `bytes`.
    fn new(bytes: &'a [u8], blocks: u32) -> Result<Self, Self::Error>;

    /// Materialize the complete raw image, copying borrowed backing when necessary.
    fn into_owned(self) -> Vec<u8>;

    /// Enumerate the saves in the raw memory card.
    fn saves(&self) -> Result<Vec<Self::Save<'_>>, Self::Error>;

    /// Decode each save individually, pairing every directory entry's identity with its save,
    /// or with `None` when the entry does not decode and its stored version must be kept.
    #[allow(clippy::type_complexity)]
    fn lenient_saves(&self) -> Result<Vec<(Self::Identity, Option<Self::Save<'_>>)>, Self::Error>;

    /// Append a save to the memory card.
    fn append_save(&mut self, save: Self::Save<'_>) -> Result<(), Self::Error>;

    fn with_saves<'s>(
        mut self,
        saves: impl IntoIterator<Item = Self::Save<'s>>,
    ) -> Result<Vec<u8>, Self::Error> {
        for save in saves {
            self.append_save(save)?;
        }
        Ok(self.into_owned())
    }
}
