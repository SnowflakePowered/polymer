//! Userspace decoding for Nintendo Switch HID reports.

const INPUT_SUBCOMMAND_REPLY: u8 = 0x21;
const INPUT_FULL_STATE: u8 = 0x30;
const INPUT_SIMPLE_STATE: u8 = 0x3f;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerKind {
    ProController,
    JoyConLeft,
    JoyConRight,
    NsoNes,
    NsoSnes,
    NsoN64,
    NsoGenesis,
}

impl ControllerKind {
    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.to_ascii_lowercase();
        if name.contains("joy-con (l)") {
            Some(Self::JoyConLeft)
        } else if name.contains("joy-con (r)") {
            Some(Self::JoyConRight)
        } else if name.contains("snes controller") {
            Some(Self::NsoSnes)
        } else if name.contains("hvc controller") || name.contains("nes controller") {
            Some(Self::NsoNes)
        } else if name.contains("n64 controller") {
            Some(Self::NsoN64)
        } else if name.contains("md/gen control pad") {
            Some(Self::NsoGenesis)
        } else if name.contains("pro controller") {
            Some(Self::ProController)
        } else {
            None
        }
    }
    pub const fn label(self) -> &'static str {
        match self {
            Self::ProController => "Nintendo Switch Pro Controller",
            Self::JoyConLeft => "Joy-Con (L)",
            Self::JoyConRight => "Joy-Con (R)",
            Self::NsoNes => "Nintendo Switch Online NES Controller",
            Self::NsoSnes => "Nintendo Switch Online SNES Controller",
            Self::NsoN64 => "Nintendo Switch Online N64 Controller",
            Self::NsoGenesis => "Nintendo Switch Online Genesis / Mega Drive Controller",
        }
    }
    const fn has_left_stick(self) -> bool {
        matches!(self, Self::ProController | Self::JoyConLeft | Self::NsoN64)
    }
    const fn has_right_stick(self) -> bool {
        matches!(self, Self::ProController | Self::JoyConRight)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Stick {
    pub x: u16,
    pub y: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Telemetry {
    pub kind: ControllerKind,
    pub report_id: u8,
    pub buttons: u32,
    pub battery_level: Option<u8>,
    pub charging: bool,
    pub left_stick: Option<Stick>,
    pub right_stick: Option<Stick>,
}

impl Telemetry {
    pub fn pressed_buttons(self) -> impl Iterator<Item = &'static str> {
        const BUTTONS: [(u32, &str); 20] = [
            (1 << 3, "A"),
            (1 << 2, "B"),
            (1 << 1, "X"),
            (1, "Y"),
            (1 << 6, "R"),
            (1 << 7, "ZR"),
            (1 << 22, "L"),
            (1 << 23, "ZL"),
            (1 << 9, "+"),
            (1 << 8, "−"),
            (1 << 12, "Home"),
            (1 << 13, "Capture"),
            (1 << 19, "Left"),
            (1 << 18, "Right"),
            (1 << 17, "Up"),
            (1 << 16, "Down"),
            (1 << 11, "L Stick"),
            (1 << 10, "R Stick"),
            (1 << 4, "SR"),
            (1 << 5, "SL"),
        ];
        BUTTONS
            .into_iter()
            .filter_map(move |(mask, name)| (self.buttons & mask != 0).then_some(name))
    }
}

pub fn parse_report(kind: ControllerKind, report: &[u8]) -> Option<Telemetry> {
    match report.first().copied()? {
        INPUT_FULL_STATE | INPUT_SUBCOMMAND_REPLY => parse_full_report(kind, report),
        INPUT_SIMPLE_STATE => parse_simple_report(kind, report),
        _ => None,
    }
}

fn parse_full_report(kind: ControllerKind, report: &[u8]) -> Option<Telemetry> {
    if report.len() < 13 {
        return None;
    }
    let buttons = u32::from(report[3]) | (u32::from(report[4]) << 8) | (u32::from(report[5]) << 16);
    Some(Telemetry {
        kind,
        report_id: report[0],
        buttons,
        battery_level: Some(report[2] >> 5),
        charging: report[2] & 0x10 != 0,
        left_stick: kind.has_left_stick().then(|| decode_stick(&report[6..9])),
        right_stick: kind.has_right_stick().then(|| decode_stick(&report[9..12])),
    })
}

fn parse_simple_report(kind: ControllerKind, report: &[u8]) -> Option<Telemetry> {
    if report.len() < 4 {
        return None;
    }
    Some(Telemetry {
        kind,
        report_id: report[0],
        buttons: u32::from(report[1]) | (u32::from(report[2]) << 8),
        battery_level: None,
        charging: false,
        left_stick: None,
        right_stick: None,
    })
}

fn decode_stick(data: &[u8]) -> Stick {
    Stick {
        x: u16::from(data[0]) | (u16::from(data[1] & 0x0f) << 8),
        y: u16::from(data[1] >> 4) | (u16::from(data[2]) << 4),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recognizes_nso_names() {
        assert_eq!(
            ControllerKind::from_name("Nintendo N64 Controller"),
            Some(ControllerKind::NsoN64)
        );
    }
    #[test]
    fn decodes_native_state() {
        let mut report = [0u8; 49];
        report[0] = 0x30;
        report[2] = 0x90;
        report[3] = 0x0c;
        report[4] = 0x12;
        report[5] = 0x0a;
        report[6..9].copy_from_slice(&[0x34, 0x52, 0x67]);
        let telemetry = parse_report(ControllerKind::NsoN64, &report).unwrap();
        assert_eq!(telemetry.buttons, 0x0a120c);
        assert_eq!(telemetry.left_stick, Some(Stick { x: 0x234, y: 0x675 }));
    }
}
