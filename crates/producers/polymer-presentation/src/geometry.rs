//! Checked geometry in a common, top-down coordinate system.

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Extent2D {
    pub width: u32,
    pub height: u32,
}

impl Extent2D {
    pub const fn area(self) -> u64 {
        self.width as u64 * self.height as u64
    }
}
