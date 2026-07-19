//! Mouse input routing and coordinate handling.

pub mod layout;

/// Physical source that produced a pointer event in the viewer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum PointerSource {
    Mouse = 1,
    Touch = 2,
}

impl TryFrom<u32> for PointerSource {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Mouse),
            2 => Ok(Self::Touch),
            _ => Err(()),
        }
    }
}

/// Pointer transition forwarded to the target process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum PointerPhase {
    Move = 1,
    ButtonDown = 2,
    ButtonUp = 3,
    Wheel = 4,
    Cancel = 5,
}

impl TryFrom<u32> for PointerPhase {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Move),
            2 => Ok(Self::ButtonDown),
            3 => Ok(Self::ButtonUp),
            4 => Ok(Self::Wheel),
            5 => Ok(Self::Cancel),
            _ => Err(()),
        }
    }
}

/// An absolute pointer event expressed in the presentation coordinate system
/// of the frame that the viewer displayed.
///
/// The receiver scales `(x, y)` from `coordinate_width × coordinate_height`
/// into the target window's current client extent. `button` uses Snowglass's
/// one-based mouse button numbering (`1` left, `2` right, `3` middle), with
/// zero meaning that the phase has no associated button. `buttons` is a bitset
/// whose bit `button - 1` records each currently pressed button.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PointerInput {
    /// Opaque identity of the displayed surface used to route this event.
    pub surface_id: u64,
    /// Real Win32 HWND that owns the target client coordinate space.
    pub target_window: i64,
    /// Frame whose composite placement was inverted for this event.
    pub frame_number: u64,
    pub source: PointerSource,
    pub phase: PointerPhase,
    /// Contact identifier. Mouse events use zero.
    pub pointer_id: u32,
    pub x: i32,
    pub y: i32,
    pub coordinate_width: u32,
    pub coordinate_height: u32,
    pub button: u32,
    pub buttons: u32,
    pub wheel_horizontal: i32,
    pub wheel_vertical: i32,
    pub timestamp: u32,
    pub sequence: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_wire_enums_reject_unknown_values() {
        assert_eq!(PointerSource::try_from(1), Ok(PointerSource::Mouse));
        assert_eq!(PointerSource::try_from(2), Ok(PointerSource::Touch));
        assert!(PointerSource::try_from(3).is_err());
        assert_eq!(PointerPhase::try_from(5), Ok(PointerPhase::Cancel));
        assert!(PointerPhase::try_from(0).is_err());
    }
}
