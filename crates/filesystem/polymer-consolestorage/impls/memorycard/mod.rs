mod raw;

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

pub trait RawMemoryCardImage<'a>: Sized {
    type Error;

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
