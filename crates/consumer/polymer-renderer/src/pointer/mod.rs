use crate::{
    CompositingPlan, MappedPointer, Presentation,
    input::map_pointer,
    layout::{Point, Rect},
};
use polymer_input::PointerSample;
use polymer_protocol::input::{PointerInput, PointerPhase, PointerSource};
use std::time::Instant;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SurfaceSelector {
    pub connection: u64,
    pub surface_id: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PointerTarget {
    #[default]
    HitTest,
    Surface(SurfaceSelector),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointerCalibration {
    matrix: [[f32; 3]; 3],
}

impl PointerCalibration {
    pub const IDENTITY: Self = Self {
        matrix: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
    };

    pub fn from_corners(observed: [[f32; 2]; 4], target: Rect) -> Option<Self> {
        let targets = [
            [target.x as f32, target.y as f32],
            [target.x as f32 + target.width as f32, target.y as f32],
            [
                target.x as f32 + target.width as f32,
                target.y as f32 + target.height as f32,
            ],
            [target.x as f32, target.y as f32 + target.height as f32],
        ];
        let mut system = [[0.0_f32; 9]; 8];
        for (index, (source, destination)) in observed.into_iter().zip(targets).enumerate() {
            let [x, y] = source;
            let [u, v] = destination;
            system[index * 2] = [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, u];
            system[index * 2 + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, v];
        }
        for column in 0..8 {
            let pivot = (column..8).max_by(|left, right| {
                system[*left][column]
                    .abs()
                    .total_cmp(&system[*right][column].abs())
            })?;
            if system[pivot][column].abs() <= f32::EPSILON {
                return None;
            }
            system.swap(column, pivot);
            let divisor = system[column][column];
            for value in &mut system[column][column..] {
                *value /= divisor;
            }
            for row in 0..8 {
                if row == column {
                    continue;
                }
                let factor = system[row][column];
                for entry in column..9 {
                    system[row][entry] -= factor * system[column][entry];
                }
            }
        }
        let values = std::array::from_fn::<_, 8, _>(|index| system[index][8]);
        Some(Self {
            matrix: [
                [values[0], values[1], values[2]],
                [values[3], values[4], values[5]],
                [values[6], values[7], 1.0],
            ],
        })
    }

    pub fn project(&self, position: [f32; 2], field_of_view: [f32; 2]) -> Point {
        let x = (position[0] - 0.5) * field_of_view[0].tan() + 0.5;
        let y = (position[1] - 0.5) * field_of_view[1].tan() + 0.5;
        let divisor = self.matrix[2][0] * x + self.matrix[2][1] * y + self.matrix[2][2];
        Point::new(
            f64::from(
                (self.matrix[0][0] * x + self.matrix[0][1] * y + self.matrix[0][2]) / divisor,
            ),
            f64::from(
                (self.matrix[1][0] * x + self.matrix[1][1] * y + self.matrix[1][2]) / divisor,
            ),
        )
    }
}

impl Default for PointerCalibration {
    fn default() -> Self {
        Self::IDENTITY
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PointerProjection {
    pub pointer: PointerInput,
    pub normalized: [i16; 2],
    pub surface_id: u64,
    pub connection: u64,
}

pub struct PointerProjector {
    cursor: Point,
    calibration: PointerCalibration,
    target: PointerTarget,
    sensitivity: f32,
    sequence: u32,
    started: Instant,
}

impl PointerProjector {
    pub fn new(started: Instant) -> Self {
        Self {
            cursor: Point::default(),
            calibration: PointerCalibration::IDENTITY,
            target: PointerTarget::HitTest,
            sensitivity: 1.0,
            sequence: 0,
            started,
        }
    }

    pub fn accept(&mut self, sample: &PointerSample, bounds: Rect, _now: Instant) {
        match *sample {
            PointerSample::None => {}
            PointerSample::Relative {
                delta,
                counts_per_unit,
            } => {
                let scale = f64::from(self.sensitivity / counts_per_unit.max(1.0));
                self.cursor.x =
                    (self.cursor.x + f64::from(delta[0]) * scale * f64::from(bounds.width)).clamp(
                        f64::from(bounds.x),
                        f64::from(bounds.x) + f64::from(bounds.width),
                    );
                self.cursor.y =
                    (self.cursor.y + f64::from(delta[1]) * scale * f64::from(bounds.height)).clamp(
                        f64::from(bounds.y),
                        f64::from(bounds.y) + f64::from(bounds.height),
                    );
            }
            PointerSample::Angular {
                position,
                field_of_view,
                ..
            } => {
                let projected = self.calibration.project(position, field_of_view);
                self.cursor = Point::new(
                    f64::from(bounds.x) + projected.x * f64::from(bounds.width),
                    f64::from(bounds.y) + projected.y * f64::from(bounds.height),
                );
            }
            PointerSample::Surface { position, .. } => {
                self.cursor = Point::new(
                    f64::from(bounds.x) + f64::from(position[0]) * f64::from(bounds.width),
                    f64::from(bounds.y) + f64::from(position[1]) * f64::from(bounds.height),
                );
            }
        }
    }

    pub fn set_cursor(&mut self, point: Point) {
        self.cursor = point;
    }

    pub fn cursor(&self) -> Point {
        self.cursor
    }

    pub fn set_target(&mut self, target: PointerTarget) {
        self.target = target;
    }

    pub fn set_calibration(&mut self, calibration: PointerCalibration) {
        self.calibration = calibration;
    }

    pub fn set_sensitivity(&mut self, sensitivity: f32) {
        self.sensitivity = sensitivity.max(0.0);
    }

    pub fn recenter(&mut self) {
        self.cursor = Point::new(0.5, 0.5);
    }

    pub fn project(
        &mut self,
        presentation: &Presentation,
        plan: &CompositingPlan,
    ) -> Option<PointerProjection> {
        let mapped = match self.target {
            PointerTarget::HitTest => map_pointer(presentation, plan, self.cursor, None),
            PointerTarget::Surface(selector) => {
                crate::input::map_pointer_to_surface(presentation, plan, self.cursor, selector)
            }
        }?;
        self.sequence = self.sequence.wrapping_add(1);
        Some(self.projection(mapped))
    }

    fn projection(&self, mapped: MappedPointer) -> PointerProjection {
        let width = mapped.stream_extent.width.max(1);
        let height = mapped.stream_extent.height.max(1);
        let x = mapped.position.x.round().clamp(0.0, f64::from(width - 1)) as i32;
        let y = mapped.position.y.round().clamp(0.0, f64::from(height - 1)) as i32;
        PointerProjection {
            pointer: PointerInput {
                surface_id: mapped.surface_id,
                target_window: mapped.target_window,
                frame_number: mapped.frame.frame_number,
                source: PointerSource::Mouse,
                phase: PointerPhase::Move,
                pointer_id: 0,
                x,
                y,
                coordinate_width: width,
                coordinate_height: height,
                button: 0,
                buttons: 0,
                wheel_horizontal: 0,
                wheel_vertical: 0,
                timestamp: self.started.elapsed().as_millis() as u32,
                sequence: self.sequence,
            },
            normalized: [
                Self::axis(mapped.normalized_position.x),
                Self::axis(mapped.normalized_position.y),
            ],
            surface_id: mapped.surface_id,
            connection: mapped.connection,
        }
    }

    fn axis(value: f64) -> i16 {
        (value.clamp(0.0, 1.0) * 65535.0 - 32768.0)
            .round()
            .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
    }
}
