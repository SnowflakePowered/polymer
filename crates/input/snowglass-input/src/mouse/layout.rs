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

/// Metadata needed to display and interact with one captured surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SurfaceLayout {
    /// Extent of the valid captured image. This determines its displayed aspect
    /// ratio independently of the allocation that contains it.
    pub image_extent: Extent,
    /// Extent of the target presentation coordinate space.
    pub presentation_extent: Extent,
    /// Rectangle where this image appeared in the target presentation.
    pub presentation_rect: Rect,
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

/// The calculated viewer placement and target mapping for one surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompositePlacement {
    /// Index into the input slice passed to [`vertical_surface_layout`].
    pub surface_index: usize,
    pub viewer_rect: Rect,
    pub presentation_extent: Extent,
    pub presentation_rect: Rect,
}

impl CompositePlacement {
    pub fn contains(self, point: Point) -> bool {
        self.viewer_rect.contains(point)
    }

    /// Maps a viewer position into the target presentation coordinate space.
    pub fn map_to_presentation(self, point: Point, outside: OutsideBehavior) -> Option<Point> {
        if self.viewer_rect.is_empty() || self.presentation_rect.is_empty() {
            return None;
        }
        if outside == OutsideBehavior::Reject && !self.contains(point) {
            return None;
        }

        let mut u = (point.x - f64::from(self.viewer_rect.x)) / f64::from(self.viewer_rect.width);
        let mut v = (point.y - f64::from(self.viewer_rect.y)) / f64::from(self.viewer_rect.height);
        if outside == OutsideBehavior::Clamp {
            u = u.clamp(0.0, 1.0);
            v = v.clamp(0.0, 1.0);
        }

        Some(Point::new(
            f64::from(self.presentation_rect.x) + u * f64::from(self.presentation_rect.width),
            f64::from(self.presentation_rect.y) + v * f64::from(self.presentation_rect.height),
        ))
    }

    /// Maps a viewer position into the current target client extent.
    ///
    /// Keeping the presentation extent in the mapping accounts for a target
    /// client whose size differs from the render or presentation resolution.
    pub fn map_to_client(
        self,
        point: Point,
        client_extent: Extent,
        outside: OutsideBehavior,
    ) -> Option<Point> {
        if self.presentation_extent.is_empty() || client_extent.is_empty() {
            return None;
        }
        let presentation = self.map_to_presentation(point, outside)?;
        Some(Point::new(
            presentation.x * f64::from(client_extent.width)
                / f64::from(self.presentation_extent.width),
            presentation.y * f64::from(client_extent.height)
                / f64::from(self.presentation_extent.height),
        ))
    }
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
    surfaces: &[SurfaceLayout],
    order: VerticalOrder,
) -> Vec<CompositePlacement> {
    if viewer_extent.is_empty() {
        return Vec::new();
    }
    let visible = surfaces
        .iter()
        .filter(|surface| !surface.image_extent.is_empty())
        .count() as u32;
    if visible == 0 {
        return Vec::new();
    }
    let row_height = viewer_extent.height / visible;
    if row_height == 0 {
        return Vec::new();
    }

    surfaces
        .iter()
        .enumerate()
        .filter(|(_, surface)| !surface.image_extent.is_empty())
        .enumerate()
        .filter_map(|(visible_index, (surface_index, surface))| {
            let visible_index = visible_index as u32;
            let row = match order {
                VerticalOrder::TopToBottom => visible_index,
                VerticalOrder::BottomToTop => visible - visible_index - 1,
            };
            let row_y = i64::from(row) * i64::from(row_height);
            let row_bounds = Rect::new(0, clamp_i64_to_i32(row_y), viewer_extent.width, row_height);
            let viewer_rect = aspect_fit(surface.image_extent, row_bounds)?;
            Some(CompositePlacement {
                surface_index,
                viewer_rect,
                presentation_extent: surface.presentation_extent,
                presentation_rect: surface.presentation_rect,
            })
        })
        .collect()
}

fn clamp_i64_to_i32(value: i64) -> i32 {
    value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn surface(image_width: u32, image_height: u32) -> SurfaceLayout {
        SurfaceLayout {
            image_extent: Extent::new(image_width, image_height),
            presentation_extent: Extent::new(1920, 1080),
            presentation_rect: Rect::new(0, 0, 1920, 1080),
        }
    }

    #[test]
    fn aspect_fit_letterboxes_without_exceeding_bounds() {
        assert_eq!(
            aspect_fit(Extent::new(1920, 1080), Rect::new(0, 0, 1000, 1000)),
            Some(Rect::new(0, 218, 1000, 563))
        );
        assert_eq!(
            aspect_fit(Extent::new(0, 1080), Rect::new(0, 0, 1000, 1000)),
            None
        );
    }

    #[test]
    fn vertical_layout_uses_equal_rows() {
        let placements = vertical_surface_layout(
            Extent::new(1200, 800),
            &[surface(1920, 1080), surface(1920, 1080)],
            VerticalOrder::TopToBottom,
        );
        assert_eq!(placements.len(), 2);
        assert_eq!(placements[0].viewer_rect, Rect::new(244, 0, 711, 400));
        assert_eq!(placements[1].viewer_rect, Rect::new(244, 400, 711, 400));
    }

    #[test]
    fn reverse_layout_changes_rows_without_changing_surface_identity() {
        let placements = vertical_surface_layout(
            Extent::new(1200, 800),
            &[surface(1920, 1080), surface(1920, 1080)],
            VerticalOrder::BottomToTop,
        );
        assert_eq!(placements[0].surface_index, 0);
        assert_eq!(placements[0].viewer_rect.y, 400);
        assert_eq!(placements[1].surface_index, 1);
        assert_eq!(placements[1].viewer_rect.y, 0);
    }

    #[test]
    fn empty_surfaces_do_not_consume_rows() {
        let placements = vertical_surface_layout(
            Extent::new(800, 600),
            &[surface(0, 0), surface(800, 600)],
            VerticalOrder::TopToBottom,
        );
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].surface_index, 1);
        assert_eq!(placements[0].viewer_rect, Rect::new(0, 0, 800, 600));
    }

    #[test]
    fn viewer_position_maps_through_the_presentation_rect() {
        let placement = CompositePlacement {
            surface_index: 0,
            viewer_rect: Rect::new(100, 50, 800, 400),
            presentation_extent: Extent::new(1920, 1080),
            presentation_rect: Rect::new(200, 100, 1600, 800),
        };
        assert_eq!(
            placement.map_to_presentation(Point::new(500.0, 250.0), OutsideBehavior::Reject),
            Some(Point::new(1000.0, 500.0))
        );
        assert_eq!(
            placement.map_to_presentation(Point::new(99.0, 250.0), OutsideBehavior::Reject),
            None
        );
    }

    #[test]
    fn presentation_position_scales_to_the_current_client() {
        let placement = CompositePlacement {
            surface_index: 0,
            viewer_rect: Rect::new(0, 0, 960, 540),
            presentation_extent: Extent::new(1920, 1080),
            presentation_rect: Rect::new(0, 0, 1920, 1080),
        };
        assert_eq!(
            placement.map_to_client(
                Point::new(480.0, 270.0),
                Extent::new(1280, 720),
                OutsideBehavior::Reject,
            ),
            Some(Point::new(640.0, 360.0))
        );
    }

    #[test]
    fn captured_pointer_positions_clamp_to_the_surface() {
        let placement = CompositePlacement {
            surface_index: 0,
            viewer_rect: Rect::new(100, 100, 400, 200),
            presentation_extent: Extent::new(800, 600),
            presentation_rect: Rect::new(50, 25, 700, 550),
        };
        assert_eq!(
            placement.map_to_presentation(Point::new(0.0, 400.0), OutsideBehavior::Clamp),
            Some(Point::new(50.0, 575.0))
        );
    }
}
