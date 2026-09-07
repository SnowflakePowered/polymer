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

    pub const fn rect(self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            extent: self,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub extent: Extent2D,
}
