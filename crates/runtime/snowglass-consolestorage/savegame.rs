pub trait RawSaveGame {
    /// The raw bytes of the raw saved game
    fn bytes(&self) -> &[u8];
}
