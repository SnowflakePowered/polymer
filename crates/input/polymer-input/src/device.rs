use std::{fmt, sync::Arc};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BackendId(&'static str);

impl BackendId {
    pub const SDL3: Self = Self("sdl3");
    pub const BLUETOOTH: Self = Self("bt");
    pub const VIRTUAL: Self = Self("virtual");

    pub const fn new(value: &'static str) -> Self {
        Self(value)
    }

    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for BackendId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DeviceId {
    backend: BackendId,
    value: Arc<str>,
}

impl DeviceId {
    pub fn new(backend: BackendId, key: &str) -> Self {
        Self {
            backend,
            value: Arc::from(format!("{}:{key}", backend.as_str())),
        }
    }

    pub const fn backend(&self) -> BackendId {
        self.backend
    }

    pub fn as_str(&self) -> &str {
        &self.value
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct PlayerIndex(u8);

impl PlayerIndex {
    pub const fn new(value: u8) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u8 {
        self.0
    }
}

impl From<PlayerIndex> for u8 {
    fn from(value: PlayerIndex) -> Self {
        value.get()
    }
}

impl TryFrom<usize> for PlayerIndex {
    type Error = ();

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        u8::try_from(value).map(Self).map_err(|_| ())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
pub enum DeviceRole {
    #[default]
    Whole,
    LeftHalf,
    RightHalf,
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct Capabilities(u32);

impl Capabilities {
    pub const RUMBLE: Self = Self(1 << 0);
    pub const HD_RUMBLE: Self = Self(1 << 1);
    pub const PLAYER_LED: Self = Self(1 << 2);
    pub const HOME_LED: Self = Self(1 << 3);
    pub const ACCELEROMETER: Self = Self(1 << 4);
    pub const GYROSCOPE: Self = Self(1 << 5);
    pub const ORIENTATION: Self = Self(1 << 6);
    pub const TOUCHPAD: Self = Self(1 << 7);
    pub const BATTERY: Self = Self(1 << 8);
    pub const RAW_HID: Self = Self(1 << 9);
    pub const RECENTER: Self = Self(1 << 10);
    pub const POINTER_RELATIVE: Self = Self(1 << 11);
    pub const POINTER_ANGULAR: Self = Self(1 << 12);
    pub const POINTER_SURFACE: Self = Self(1 << 13);
    pub const POINTER_CONTACT: Self = Self(1 << 14);
    pub const CALIBRATE_STICK_CENTER: Self = Self(1 << 16);
    pub const CALIBRATE_STICK_RANGE: Self = Self(1 << 17);
    pub const CALIBRATE_DEAD_ZONE: Self = Self(1 << 18);
    pub const CALIBRATE_TRIGGER: Self = Self(1 << 19);
    pub const CALIBRATE_MOTION_BIAS: Self = Self(1 << 20);
    pub const CALIBRATE_MOTION_SCALE: Self = Self(1 << 21);
    pub const CALIBRATE_POINTER: Self = Self(1 << 22);
    pub const POINTER_ABSOLUTE: Self = Self(Self::POINTER_ANGULAR.0 | Self::POINTER_SURFACE.0);
    pub const POINTER: Self = Self(Self::POINTER_RELATIVE.0 | Self::POINTER_ABSOLUTE.0);

    pub const fn from_bits_retain(bits: u32) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn has_pointer(self) -> bool {
        self.intersects(Self::POINTER)
    }
}

impl std::ops::BitOr for Capabilities {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        self.union(rhs)
    }
}

impl std::ops::BitOrAssign for Capabilities {
    fn bitor_assign(&mut self, rhs: Self) {
        *self = self.union(rhs);
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
pub struct BatteryState {
    pub level: Option<u8>,
    pub charging: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeviceDescriptor {
    pub id: DeviceId,
    pub name: Arc<str>,
    pub profile: Option<Arc<str>>,
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
    pub serial: Option<Uuid>,
    pub role: DeviceRole,
    pub capabilities: Capabilities,
}

impl DeviceDescriptor {
    /// Converts a backend-provided stable identifier into Polymer's
    /// backend-neutral device serial namespace.
    pub fn serial_uuid(serial: &str) -> Option<Uuid> {
        let serial = serial.trim();
        if serial.is_empty() {
            return None;
        }
        let name = format!("polymer:input:serial:{}", serial.to_ascii_uppercase());
        Some(Uuid::new_v5(&Uuid::NAMESPACE_URL, name.as_bytes()))
    }
}
