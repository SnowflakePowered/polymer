pub trait VirtualMemoryCard {
    const BYTES_PER_BLOCK: u32;
    const BLOCK_SIZES: &'static [u32];
}
