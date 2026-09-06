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

impl Rect {
    pub fn intersect(self, other: Self) -> Option<Self> {
        let left = i64::from(self.x).max(i64::from(other.x));
        let top = i64::from(self.y).max(i64::from(other.y));
        let right = (i64::from(self.x) + i64::from(self.extent.width))
            .min(i64::from(other.x) + i64::from(other.extent.width));
        let bottom = (i64::from(self.y) + i64::from(self.extent.height))
            .min(i64::from(other.y) + i64::from(other.extent.height));
        if right <= left || bottom <= top {
            return None;
        }
        Some(Self {
            x: left.try_into().ok()?,
            y: top.try_into().ok()?,
            extent: Extent2D {
                width: (right - left).try_into().ok()?,
                height: (bottom - top).try_into().ok()?,
            },
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegionMapping {
    pub source: Rect,
    pub destination: Rect,
    pub flip_x: bool,
    pub flip_y: bool,
}

impl RegionMapping {
    /// Maps a clipped rectangle, rounding outwards to retain covered pixels.
    pub fn map(self, rect: Rect) -> Option<Rect> {
        let rect = rect.intersect(self.source)?;
        let (x, width) = Self::axis(
            rect.x,
            rect.extent.width,
            self.source.x,
            self.source.extent.width,
            self.destination.x,
            self.destination.extent.width,
            self.flip_x,
        )?;
        let (y, height) = Self::axis(
            rect.y,
            rect.extent.height,
            self.source.y,
            self.source.extent.height,
            self.destination.y,
            self.destination.extent.height,
            self.flip_y,
        )?;
        Some(Rect {
            x,
            y,
            extent: Extent2D { width, height },
        })
    }

    fn axis(
        position: i32,
        size: u32,
        origin: i32,
        source_size: u32,
        destination: i32,
        destination_size: u32,
        flipped: bool,
    ) -> Option<(i32, u32)> {
        if source_size == 0 || destination_size == 0 {
            return None;
        }
        let mut left = i128::from(position) - i128::from(origin);
        let mut right = left + i128::from(size);
        if flipped {
            (left, right) = (
                i128::from(source_size) - right,
                i128::from(source_size) - left,
            );
        }
        let scale = i128::from(destination_size);
        let divisor = i128::from(source_size);
        let lower = left * scale / divisor + i128::from(destination);
        let upper = (right * scale + divisor - 1) / divisor + i128::from(destination);
        Some((lower.try_into().ok()?, (upper - lower).try_into().ok()?))
    }

    pub const fn inverse(self) -> Self {
        Self {
            source: self.destination,
            destination: self.source,
            flip_x: self.flip_x,
            flip_y: self.flip_y,
        }
    }
}
