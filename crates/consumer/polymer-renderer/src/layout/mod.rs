//! Backend-neutral placement and pointer-coordinate calculations.
//!
//! A graphics consumer uses this module to calculate where captured images are
//! drawn. Input routing then uses the same placements to invert that operation,
//! avoiding a second approximation of the compositor layout.

/// A two-dimensional pixel extent.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Extent {
    pub width: u32,
    pub height: u32,
}

impl Extent {
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// An axis-aligned pixel rectangle with a signed origin.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub const fn extent(self) -> Extent {
        Extent::new(self.width, self.height)
    }

    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Tests a continuous pixel position against the half-open rectangle.
    pub fn contains(self, point: Point) -> bool {
        !self.is_empty()
            && point.x >= f64::from(self.x)
            && point.y >= f64::from(self.y)
            && point.x < f64::from(self.x) + f64::from(self.width)
            && point.y < f64::from(self.y) + f64::from(self.height)
    }
}

/// A continuous position in a pixel coordinate space.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// The vertical ordering of surfaces in the viewer.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum VerticalOrder {
    #[default]
    TopToBottom,
    BottomToTop,
}

/// How pointer mapping treats positions outside a surface's viewer rectangle.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OutsideBehavior {
    /// Reject the position. This is appropriate for hover and pointer-down hit
    /// testing.
    #[default]
    Reject,
    /// Clamp the position to the surface. This is appropriate while a pointer
    /// is captured between button-down and button-up.
    Clamp,
}

/// The calculated viewer placement for one surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompositePlacement {
    /// Index into the input slice passed to [`vertical_surface_layout`].
    pub surface_index: usize,
    pub viewer_rect: Rect,
}

/// Aspect-fits `source` into `bounds`, returning integer pixels suitable for
/// both rendering and subsequent hit testing.
pub fn aspect_fit(source: Extent, bounds: Rect) -> Option<Rect> {
    if source.is_empty() || bounds.is_empty() {
        return None;
    }
    let scale = (f64::from(bounds.width) / f64::from(source.width))
        .min(f64::from(bounds.height) / f64::from(source.height));
    let width = (f64::from(source.width) * scale)
        .round()
        .clamp(1.0, f64::from(bounds.width)) as u32;
    let height = (f64::from(source.height) * scale)
        .round()
        .clamp(1.0, f64::from(bounds.height)) as u32;
    let x = i64::from(bounds.x) + i64::from((bounds.width - width) / 2);
    let y = i64::from(bounds.y) + i64::from((bounds.height - height) / 2);
    Some(Rect::new(
        clamp_i64_to_i32(x),
        clamp_i64_to_i32(y),
        width,
        height,
    ))
}

/// Lays non-empty surfaces out in equal-height rows and aspect-fits each image
/// into its row.
///
/// Empty images are omitted and do not consume a row. The returned placement's
/// `surface_index` preserves its association with the original input slice.
pub fn vertical_surface_layout(
    viewer_extent: Extent,
    image_extents: &[Extent],
    order: VerticalOrder,
) -> Vec<CompositePlacement> {
    if viewer_extent.is_empty() {
        return Vec::new();
    }
    let visible = image_extents
        .iter()
        .filter(|extent| !extent.is_empty())
        .count() as u32;
    if visible == 0 {
        return Vec::new();
    }
    let row_height = viewer_extent.height / visible;
    if row_height == 0 {
        return Vec::new();
    }

    image_extents
        .iter()
        .enumerate()
        .filter(|(_, extent)| !extent.is_empty())
        .enumerate()
        .filter_map(|(visible_index, (surface_index, &image_extent))| {
            let visible_index = visible_index as u32;
            let row = match order {
                VerticalOrder::TopToBottom => visible_index,
                VerticalOrder::BottomToTop => visible - visible_index - 1,
            };
            let row_y = i64::from(row) * i64::from(row_height);
            let row_bounds = Rect::new(0, clamp_i64_to_i32(row_y), viewer_extent.width, row_height);
            let viewer_rect = aspect_fit(image_extent, row_bounds)?;
            Some(CompositePlacement {
                surface_index,
                viewer_rect,
            })
        })
        .collect()
}

fn clamp_i64_to_i32(value: i64) -> i32 {
    value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}
