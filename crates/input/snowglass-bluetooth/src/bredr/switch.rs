//! Nintendo Switch 1 BR/EDR controller protocol.

use std::time::Duration;

use tokio::sync::broadcast;

use crate::{Error, ReportReceiver, Result, Transport};

use super::BrEdrHid;

pub const NINTENDO_VENDOR_ID: u16 = 0x057e;
pub const SNES_PRODUCT_ID: u16 = 0x2017;
pub const N64_PRODUCT_ID: u16 = 0x2019;
pub const GENESIS_PRODUCT_ID: u16 = 0x201e;

const OUTPUT_RUMBLE_AND_SUBCOMMAND: u8 = 0x01;
const INPUT_SUBCOMMAND_REPLY: u8 = 0x21;
const INPUT_FULL_STATE: u8 = 0x30;
const INPUT_SIMPLE_STATE: u8 = 0x3f;
const SUBCOMMAND_SET_REPORT_MODE: u8 = 0x03;
const SUBCOMMAND_SET_HCI_STATE: u8 = 0x06;
const SUBCOMMAND_SET_PLAYER_LIGHTS: u8 = 0x30;
const SUBCOMMAND_ENABLE_VIBRATION: u8 = 0x48;
const FULL_REPORT_MODE: u8 = 0x30;
const NEUTRAL_RUMBLE: [u8; 4] = [0x00, 0x01, 0x40, 0x40];

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

    pub const fn is_nso(self) -> bool {
        matches!(
            self,
            Self::NsoNes | Self::NsoSnes | Self::NsoN64 | Self::NsoGenesis
        )
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
            (1 << 0, "Y"),
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

/// Initializes continuous native reports, vibration, and the player LED. The
/// packet counter is global across all subcommands as required by Nintendo's
/// protocol.
pub async fn initialize(transport: &mut BrEdrHid, kind: ControllerKind, slot: usize) -> Result<u8> {
    let mut reports = transport.reports();
    let mut packet_counter = 0u8;
    send_subcommand_with_retry(
        transport,
        &mut reports,
        &mut packet_counter,
        SUBCOMMAND_SET_REPORT_MODE,
        &[FULL_REPORT_MODE],
    )
    .await?;
    send_subcommand_with_retry(
        transport,
        &mut reports,
        &mut packet_counter,
        SUBCOMMAND_ENABLE_VIBRATION,
        &[0x01],
    )
    .await?;
    send_subcommand_with_retry(
        transport,
        &mut reports,
        &mut packet_counter,
        SUBCOMMAND_SET_PLAYER_LIGHTS,
        &[1 << (slot % 4)],
    )
    .await?;

    tracing::debug!(controller = kind.label(), "initialized Switch controller");
    Ok(packet_counter)
}

/// Asks a Switch 1 controller to disconnect into its normal sleep/page state.
/// A later controller button press then starts the standard device-initiated
/// reconnect sequence instead of immediately repairing a host-torn ACL.
pub async fn request_sleep(transport: &mut BrEdrHid, packet_counter: &mut u8) -> Result<()> {
    // Bonded Switch links may perform several encryption pause/resume cycles
    // after reconnect. Let that controller-driven maintenance finish before
    // changing HCI state, otherwise the following button wake can stall after
    // an LMP transaction collision. The observed cadence is about one second.
    transport
        .wait_for_security_quiet(Duration::from_millis(1_200))
        .await?;
    let report = subcommand_report(*packet_counter, SUBCOMMAND_SET_HCI_STATE, &[0x00]);
    *packet_counter = (*packet_counter + 1) & 0x0f;
    transport.send(&report).await?;
    // The controller may drop the ACL before acknowledging subcommand 0x06.
    tokio::time::sleep(Duration::from_millis(100)).await;
    Ok(())
}

async fn send_subcommand_with_retry(
    transport: &mut BrEdrHid,
    reports: &mut ReportReceiver,
    packet_counter: &mut u8,
    subcommand: u8,
    data: &[u8],
) -> Result<()> {
    for attempt in 0..3 {
        let report = subcommand_report(*packet_counter, subcommand, data);
        *packet_counter = (*packet_counter + 1) & 0x0f;
        transport.send(&report).await?;
        match wait_for_subcommand_reply(reports, subcommand).await {
            Ok(()) => return Ok(()),
            Err(error) if attempt != 2 => {
                tracing::debug!(subcommand, attempt, %error, "retrying Switch subcommand");
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!()
}

fn subcommand_report(packet_counter: u8, subcommand: u8, data: &[u8]) -> [u8; 49] {
    debug_assert!(data.len() <= 38);
    let mut report = [0u8; 49];
    report[0] = OUTPUT_RUMBLE_AND_SUBCOMMAND;
    report[1] = packet_counter & 0x0f;
    report[2..6].copy_from_slice(&NEUTRAL_RUMBLE);
    report[6..10].copy_from_slice(&NEUTRAL_RUMBLE);
    report[10] = subcommand;
    report[11..11 + data.len()].copy_from_slice(data);
    report
}

async fn wait_for_subcommand_reply(reports: &mut ReportReceiver, subcommand: u8) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match reports.recv().await {
                Ok(report)
                    if report.first() == Some(&INPUT_SUBCOMMAND_REPLY)
                        && report.len() >= 15
                        && report[14] == subcommand =>
                {
                    return if report[13] & 0x80 != 0 {
                        Ok(())
                    } else {
                        Err(Error::L2cap(format!(
                            "Switch subcommand 0x{subcommand:02x} was rejected"
                        )))
                    };
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return Err(Error::ChannelClosed),
            }
        }
    })
    .await
    .map_err(|_| Error::Timeout("Switch subcommand reply"))?
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
    fn recognizes_nso_controller_names() {
        assert_eq!(
            ControllerKind::from_name("SNES Controller"),
            Some(ControllerKind::NsoSnes)
        );
        assert_eq!(
            ControllerKind::from_name("Nintendo N64 Controller"),
            Some(ControllerKind::NsoN64)
        );
        assert_eq!(
            ControllerKind::from_name("HVC Controller (L)"),
            Some(ControllerKind::NsoNes)
        );
        assert_eq!(
            ControllerKind::from_name("MD/Gen Control Pad"),
            Some(ControllerKind::NsoGenesis)
        );
    }

    #[test]
    fn builds_counted_full_report_subcommand() {
        let report = subcommand_report(0x12, SUBCOMMAND_SET_REPORT_MODE, &[0x30]);
        assert_eq!(
            report[0..11],
            [
                0x01, 0x02, 0x00, 0x01, 0x40, 0x40, 0x00, 0x01, 0x40, 0x40, 0x03
            ]
        );
        assert_eq!(report[11], 0x30);
        assert!(report[12..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn decodes_native_n64_state() {
        let mut report = [0u8; 49];
        report[0] = 0x30;
        report[2] = 0x90;
        report[3] = 0x0c; // A + B
        report[4] = 0x12; // Plus + Home
        report[5] = 0x0a; // Up + Left
        report[6..9].copy_from_slice(&[0x34, 0x52, 0x67]);
        let telemetry = parse_report(ControllerKind::NsoN64, &report).unwrap();
        assert_eq!(telemetry.battery_level, Some(4));
        assert!(telemetry.charging);
        assert_eq!(telemetry.buttons, 0x0a120c);
        assert_eq!(telemetry.left_stick, Some(Stick { x: 0x234, y: 0x675 }));
        assert_eq!(
            telemetry.pressed_buttons().collect::<Vec<_>>(),
            ["A", "B", "+", "Home", "Left", "Up"]
        );
    }
}
