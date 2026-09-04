use eframe::egui;

#[derive(Clone, Copy)]
pub(super) struct Vector3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vector3 {
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    pub fn from_array([x, y, z]: [f32; 3]) -> Self {
        Self { x, y, z }
    }

    pub fn cross(self, rhs: Self) -> Self {
        Self {
            x: self.y * rhs.z - self.z * rhs.y,
            y: self.z * rhs.x - self.x * rhs.z,
            z: self.x * rhs.y - self.y * rhs.x,
        }
    }

    pub fn dot(self, rhs: Self) -> f32 {
        self.x * rhs.x + self.y * rhs.y + self.z * rhs.z
    }

    pub fn normalized(self) -> Self {
        let length = self.dot(self).sqrt();
        Self::new(self.x / length, self.y / length, self.z / length)
    }
}

#[derive(Clone, Copy)]
pub(super) struct Quaternion {
    w: f32,
    x: f32,
    y: f32,
    z: f32,
}

impl Quaternion {
    pub fn from_array([w, x, y, z]: [f32; 4]) -> Self {
        Self { w, x, y, z }.normalized()
    }

    fn normalized(self) -> Self {
        let norm = (self.w * self.w + self.x * self.x + self.y * self.y + self.z * self.z).sqrt();
        Self {
            w: self.w / norm,
            x: self.x / norm,
            y: self.y / norm,
            z: self.z / norm,
        }
    }

    pub fn conjugate(self) -> Self {
        Self {
            w: self.w,
            x: -self.x,
            y: -self.y,
            z: -self.z,
        }
    }

    pub fn multiplied(self, rhs: Self) -> Self {
        Self {
            w: self.w * rhs.w - self.x * rhs.x - self.y * rhs.y - self.z * rhs.z,
            x: self.w * rhs.x + self.x * rhs.w + self.y * rhs.z - self.z * rhs.y,
            y: self.w * rhs.y - self.x * rhs.z + self.y * rhs.w + self.z * rhs.x,
            z: self.w * rhs.z + self.x * rhs.y - self.y * rhs.x + self.z * rhs.w,
        }
        .normalized()
    }

    pub fn rotate(self, vector: Vector3) -> Vector3 {
        let quaternion_vector = Vector3::new(self.x, self.y, self.z);
        let first_cross = quaternion_vector.cross(vector);
        let second_cross = quaternion_vector.cross(first_cross);
        Vector3::new(
            vector.x + 2.0 * (self.w * first_cross.x + second_cross.x),
            vector.y + 2.0 * (self.w * first_cross.y + second_cross.y),
            vector.z + 2.0 * (self.w * first_cross.z + second_cross.z),
        )
    }

    pub fn physical_euler_degrees(self) -> [f32; 3] {
        let pitch_x = (2.0 * (self.w * self.x + self.y * self.z))
            .atan2(1.0 - 2.0 * (self.x * self.x + self.y * self.y));
        let roll_y = (2.0 * (self.w * self.y - self.z * self.x))
            .clamp(-1.0, 1.0)
            .asin();
        let yaw_z = (2.0 * (self.w * self.z + self.x * self.y))
            .atan2(1.0 - 2.0 * (self.y * self.y + self.z * self.z));
        [
            pitch_x.to_degrees(),
            roll_y.to_degrees(),
            yaw_z.to_degrees(),
        ]
    }
}

pub(super) struct Projector {
    center: egui::Pos2,
    scale: f32,
}

impl Projector {
    pub fn new(center: egui::Pos2, scale: f32) -> Self {
        Self { center, scale }
    }

    pub fn project(&self, point: Vector3) -> egui::Pos2 {
        egui::pos2(
            self.center.x + (point.x + point.y * 0.22) * self.scale,
            self.center.y - (point.z * 0.78 + point.y * 0.48) * self.scale,
        )
    }

    pub fn depth(&self, point: Vector3) -> f32 {
        point.y * 0.78 - point.z * 0.62
    }
}
