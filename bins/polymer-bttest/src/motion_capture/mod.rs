use std::{
    fs::{File, create_dir_all},
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::json;

pub mod class_c_acceleration;
pub mod class_d;
pub mod lanes;
pub mod quaternion;
pub mod rare_timing;
pub mod timing;

pub const COMMON_REPORT: u8 = 0x05;
pub const GAMECUBE_REPORT: u8 = 0x0a;
pub const COMMON_INPUT_UUID: &str = "ab7de9be-89fe-49ad-828f-118f09df7fd2";
pub const GAMECUBE_INPUT_UUID: &str = "8261cba1-9435-420c-84d6-f0c75a2c8e4d";
const MOTION_LENGTH_OFFSET: usize = 14;
const MOTION_DATA_OFFSET: usize = 15;
const FULL_MOTION_LENGTH: usize = 40;
const MOTION_HEADER_LENGTH: usize = 4;
const SECOND_MODE_BYTE_OFFSET: usize = 16;

pub const PHASES: &[CapturePhase] = &[
    CapturePhase::new(
        "stationary_face_up",
        "Lay the controller face-up and keep it still.",
    ),
    CapturePhase::new(
        "stationary_face_down",
        "Lay the controller face-down and keep it still.",
    ),
    CapturePhase::new(
        "stationary_left_edge_down",
        "Rest it on its left edge and keep it still.",
    ),
    CapturePhase::new(
        "stationary_right_edge_down",
        "Rest it on its right edge and keep it still.",
    ),
    CapturePhase::new(
        "stationary_grip_edge_down",
        "Hold it vertically with the two handle tips pointing toward the floor and keep it still.",
    ),
    CapturePhase::new(
        "stationary_shoulder_edge_down",
        "Hold it vertically upside-down, with the L/R shoulder and cable edge pointing toward the floor, and keep it still.",
    ),
    CapturePhase::new(
        "rotate_pitch_positive_slow",
        "Slowly rotate 360 degrees in positive pitch over about five seconds.",
    ),
    CapturePhase::new(
        "rotate_pitch_negative_slow",
        "Slowly rotate 360 degrees in negative pitch over about five seconds.",
    ),
    CapturePhase::new(
        "rotate_roll_positive_slow",
        "Slowly rotate 360 degrees in positive roll over about five seconds.",
    ),
    CapturePhase::new(
        "rotate_roll_negative_slow",
        "Slowly rotate 360 degrees in negative roll over about five seconds.",
    ),
    CapturePhase::new(
        "rotate_yaw_positive_slow",
        "Slowly rotate 360 degrees in positive yaw over about five seconds.",
    ),
    CapturePhase::new(
        "rotate_yaw_negative_slow",
        "Slowly rotate 360 degrees in negative yaw over about five seconds.",
    ),
    CapturePhase::new(
        "flick_pitch_both_directions",
        "Make several fast pitch flicks in both directions, returning to rest between them.",
    ),
    CapturePhase::new(
        "flick_roll_both_directions",
        "Make several fast roll flicks in both directions, returning to rest between them.",
    ),
    CapturePhase::new(
        "flick_yaw_both_directions",
        "Make several fast yaw flicks in both directions, returning to rest between them.",
    ),
    CapturePhase::new(
        "isolated_taps",
        "Leave it still and make several isolated taps, with two seconds of rest between taps.",
    ),
    CapturePhase::new(
        "stationary_long",
        "Leave the controller completely untouched for at least 30 seconds.",
    ),
];

#[derive(Clone, Copy)]
pub struct CapturePhase {
    pub label: &'static str,
    pub instruction: &'static str,
}

impl CapturePhase {
    const fn new(label: &'static str, instruction: &'static str) -> Self {
        Self { label, instruction }
    }
}

pub struct MotionCapture {
    address: String,
    path: PathBuf,
    writer: BufWriter<File>,
    started_at: Instant,
    label: String,
    selected_report: u8,
    samples: u64,
    common_samples: u64,
    native_samples: u64,
    observed_report: Option<u8>,
    primary_mode_counts: [u64; 4],
    secondary_mode_counts: [u64; 4],
}

impl MotionCapture {
    pub fn start(address: &str) -> io::Result<Self> {
        let directory = capture_directory();
        create_dir_all(&directory)?;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let address_token = address.replace(':', "-");
        let path = directory.join(format!(
            "switch2-gamecube-motion-{timestamp}-{address_token}.jsonl"
        ));
        let file = File::create(&path)?;
        let mut capture = Self {
            address: address.to_owned(),
            path,
            writer: BufWriter::new(file),
            started_at: Instant::now(),
            label: "unlabeled".into(),
            selected_report: GAMECUBE_REPORT,
            samples: 0,
            common_samples: 0,
            native_samples: 0,
            observed_report: None,
            primary_mode_counts: [0; 4],
            secondary_mode_counts: [0; 4],
        };
        capture.write_json(&json!({
            "type": "header",
            "schema": "polymer.switch2-motion.v2",
            "controller": "nso-switch2-gamecube",
            "address": capture.address,
            "started_unix_ms": timestamp,
            "common_report": format!("0x{COMMON_REPORT:02x}"),
            "native_report": format!("0x{GAMECUBE_REPORT:02x}"),
            "characteristics": {
                "0x05": COMMON_INPUT_UUID,
                "0x0a": GAMECUBE_INPUT_UUID,
            },
            "timestamp": "host monotonic nanoseconds since this header",
            "payload": "complete BLE notification value, hexadecimal; Bluetooth omits HID report ID",
        }))?;
        capture.mark("capture_started")?;
        Ok(capture)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    pub const fn samples(&self) -> u64 {
        self.samples
    }

    pub const fn common_samples(&self) -> u64 {
        self.common_samples
    }

    pub const fn native_samples(&self) -> u64 {
        self.native_samples
    }

    pub const fn selected_report(&self) -> u8 {
        self.selected_report
    }

    pub const fn observed_report(&self) -> Option<u8> {
        self.observed_report
    }

    pub const fn primary_mode_counts(&self) -> [u64; 4] {
        self.primary_mode_counts
    }

    pub const fn secondary_mode_counts(&self) -> [u64; 4] {
        self.secondary_mode_counts
    }

    pub fn set_selected_report(&mut self, report_id: u8) -> io::Result<()> {
        self.selected_report = report_id;
        self.write_json(&json!({
            "type": "report_mode",
            "t_ns": self.started_at.elapsed().as_nanos(),
            "report_id": format!("0x{report_id:02x}"),
        }))
    }

    pub fn set_label(&mut self, label: &str) -> io::Result<()> {
        self.label.clear();
        self.label.push_str(label);
        self.mark("phase_changed")
    }

    pub fn mark(&mut self, event: &str) -> io::Result<()> {
        self.write_json(&json!({
            "type": "marker",
            "t_ns": self.started_at.elapsed().as_nanos(),
            "event": event,
            "label": self.label,
            "selected_report": format!("0x{:02x}", self.selected_report),
        }))
    }

    pub fn mark_experiment(&mut self, event: &str) -> io::Result<()> {
        self.write_json(&json!({
            "type": "experiment",
            "t_ns": self.started_at.elapsed().as_nanos(),
            "event": event,
            "label": self.label,
            "selected_report": format!("0x{:02x}", self.selected_report),
            "common_samples": self.common_samples,
            "native_samples": self.native_samples,
            "primary_mode_counts": self.primary_mode_counts,
            "secondary_mode_counts": self.secondary_mode_counts,
        }))
    }

    pub fn mark_transport(&mut self, event: &str, reason: Option<&str>) -> io::Result<()> {
        self.write_json(&json!({
            "type": "transport",
            "t_ns": self.started_at.elapsed().as_nanos(),
            "event": event,
            "reason": reason,
            "label": self.label,
            "selected_report": format!("0x{:02x}", self.selected_report),
        }))
    }

    pub fn mark_lane_experiment(
        &mut self,
        marker: lanes::LaneExperimentMarker<'_>,
    ) -> io::Result<()> {
        self.write_json(&json!({
            "type": "lane_experiment",
            "t_ns": self.started_at.elapsed().as_nanos(),
            "event": marker.event,
            "label": self.label,
            "selected_report": format!("0x{:02x}", self.selected_report),
            "action": marker.action,
            "action_number": marker.action_number,
            "action_count": marker.action_count,
            "repetition": marker.repetition,
            "repetition_count": marker.repetition_count,
            "boundary": marker.boundary.map(|packet| json!({
                "notification_sequence": self.samples,
                "sensor_timestamp": packet.sensor_timestamp,
                "elapsed_ticks": packet.elapsed_ticks,
                "lane_control": packet.lane_control,
                "leading_lanes": encode_hex(&packet.leading_lanes),
                "accelerometer_q16_16": packet.accelerometer_q16_16,
                "temperature_raw": packet.temperature_raw,
            })),
        }))
    }

    pub fn mark_class_d_experiment(
        &mut self,
        marker: class_d::ClassDExperimentMarker<'_>,
    ) -> io::Result<()> {
        self.write_json(&json!({
            "type": "class_d_experiment",
            "t_ns": self.started_at.elapsed().as_nanos(),
            "event": marker.event,
            "label": self.label,
            "selected_report": format!("0x{:02x}", self.selected_report),
            "action": marker.action,
            "action_number": marker.action_number,
            "action_count": marker.action_count,
            "observed": marker.observed,
            "target": marker.target,
            "packet": marker.packet.map(|packet| json!({
                "notification_sequence": self.samples,
                "sensor_timestamp": packet.sensor_timestamp,
                "elapsed_ticks": packet.elapsed_ticks,
                "range_flags": packet.range_flags,
                "omitted_component": packet.omitted_component,
                "reserved_bit_4": packet.reserved_bit_4,
                "accelerometer_first": packet.accelerometer_first,
                "gyroscope_terminal": packet.gyroscope_terminal,
                "accelerometer_terminal": packet.accelerometer_terminal,
                "temperature_raw": packet.temperature_raw,
                "unresolved_prefix_82_bits": encode_hex(&packet.unresolved_prefix),
                "unresolved_interleaved_bytes": packet.unresolved_interleaved,
                "body": encode_hex(&packet.body),
            })),
        }))
    }

    pub fn mark_class_c_acceleration_experiment(
        &mut self,
        marker: class_c_acceleration::AccelerationExperimentMarker,
    ) -> io::Result<()> {
        self.write_json(&json!({
            "type": "class_c_acceleration_experiment",
            "t_ns": self.started_at.elapsed().as_nanos(),
            "event": marker.event,
            "label": self.label,
            "selected_report": format!("0x{:02x}", self.selected_report),
            "action": marker.action.id,
            "action_title": marker.action.title,
            "action_number": marker.action_number,
            "action_count": marker.action_count,
            "observed": marker.observed,
            "target": marker.action.target,
            "total_observed": marker.total_observed,
            "rejected": marker.rejected,
            "completed_by_action": marker.completed_by_action,
            "context": marker.context.map(|context| json!({
                "previous": {
                    "notification_sequence": context.previous.notification_sequence,
                    "sensor_timestamp": context.previous.packet.sensor_timestamp,
                    "elapsed_ticks": context.previous.packet.elapsed_ticks,
                    "range_flags": context.previous.packet.range_flags,
                    "accelerometer_earlier": context.previous.packet.earlier,
                    "accelerometer_middle": context.previous.packet.middle,
                    "accelerometer_terminal": context.previous.packet.terminal,
                },
                "focus": {
                    "notification_sequence": context.focus.notification_sequence,
                    "sensor_timestamp": context.focus.packet.sensor_timestamp,
                    "elapsed_ticks": context.focus.packet.elapsed_ticks,
                    "accelerometer_q16_16": context.focus.packet.accelerometer_q16_16,
                    "temperature_raw": context.focus.packet.temperature_raw,
                },
                "next": {
                    "notification_sequence": context.next.notification_sequence,
                    "sensor_timestamp": context.next.packet.sensor_timestamp,
                    "elapsed_ticks": context.next.packet.elapsed_ticks,
                    "range_flags": context.next.packet.range_flags,
                    "accelerometer_earlier": context.next.packet.earlier,
                    "accelerometer_middle": context.next.packet.middle,
                    "accelerometer_terminal": context.next.packet.terminal,
                },
                "normal_span_ticks": context.normal_span_ticks,
                "first_sample_tick": context.first_sample_tick,
                "second_sample_tick": context.second_sample_tick,
                "previous_to_focus_sensor_ticks": context.previous_to_focus_sensor_ticks,
                "focus_to_next_sensor_ticks": context.focus_to_next_sensor_ticks,
                "previous_to_focus_host_ns": context.previous_to_focus_host_ns,
                "focus_to_next_host_ns": context.focus_to_next_host_ns,
            })),
        }))
    }

    pub fn mark_timing_experiment(
        &mut self,
        marker: timing::TimingExperimentMarker,
    ) -> io::Result<()> {
        self.write_json(&json!({
            "type": "timing_experiment",
            "t_ns": self.started_at.elapsed().as_nanos(),
            "event": marker.event,
            "label": self.label,
            "selected_report": format!("0x{:02x}", self.selected_report),
            "action": marker.action,
            "action_number": marker.action_number,
            "action_count": marker.action_count,
            "observed": marker.observed,
            "target": marker.target,
            "bit_287_counts": marker.bit_287_counts,
            "normal_elapsed_outliers": marker.elapsed_outliers,
            "context": marker.context.map(|context| json!({
                "previous": context.previous.map(timing::TimingPoint::json),
                "focus": context.focus.json(),
                "next": context.next.json(),
                "previous_to_focus_host_ns": context.previous_to_focus_host_ns,
                "focus_to_next_host_ns": context.focus_to_next_host_ns,
                "previous_to_focus_sensor_ticks": context.previous_to_focus_sensor_ticks,
                "focus_to_next_sensor_ticks": context.focus_to_next_sensor_ticks,
            })),
        }))
    }

    pub fn mark_rare_timing_experiment(
        &mut self,
        marker: rare_timing::RareTimingExperimentMarker,
    ) -> io::Result<()> {
        self.write_json(&json!({
            "type": "rare_timing_experiment",
            "t_ns": self.started_at.elapsed().as_nanos(),
            "event": marker.event,
            "label": self.label,
            "selected_report": format!("0x{:02x}", self.selected_report),
            "axis": marker.axis.map(rare_timing::MotionAxis::label),
            "target_elapsed_ticks": rare_timing::TARGET_ELAPSED_TICKS,
            "target_contexts_per_axis": rare_timing::TARGET_CONTEXTS_PER_AXIS,
            "completed_contexts": marker.completed_contexts,
            "rejected_contexts": marker.rejected_contexts,
            "observed_candidates": {
                "pitch": marker.observed_candidates[0],
                "roll": marker.observed_candidates[1],
                "yaw": marker.observed_candidates[2],
            },
            "completed_by_axis": {
                "pitch": marker.completed_by_axis[0],
                "roll": marker.completed_by_axis[1],
                "yaw": marker.completed_by_axis[2],
            },
            "context": marker.context.map(|context| json!({
                "before": context.before.map(timing::TimingPoint::json),
                "bridge_before": context.bridge_before.into_iter().map(timing::TimingPoint::json).collect::<Vec<_>>(),
                "focus": context.focus.json(),
                "after": context.after.map(timing::TimingPoint::json),
                "bridge_after": context.bridge_after.into_iter().map(timing::TimingPoint::json).collect::<Vec<_>>(),
                "continuation": context.continuation.map(timing::TimingPoint::json),
                "timeline": context.timeline.into_iter().map(timing::TimingPoint::json).collect::<Vec<_>>(),
                "host_deltas_ns": context.host_deltas_ns,
                "sensor_deltas_ticks": context.sensor_deltas_ticks,
            })),
        }))
    }

    pub fn record_notification(
        &mut self,
        address: &str,
        characteristic: &str,
        bytes: &[u8],
        received_at: Instant,
    ) -> io::Result<()> {
        if address != self.address {
            return Ok(());
        }
        let report_id = match characteristic {
            COMMON_INPUT_UUID => {
                self.common_samples += 1;
                Some(COMMON_REPORT)
            }
            GAMECUBE_INPUT_UUID => {
                self.native_samples += 1;
                Some(GAMECUBE_REPORT)
            }
            _ => None,
        };
        let adaptive_modes = (report_id == Some(GAMECUBE_REPORT))
            .then(|| native_adaptive_modes(bytes))
            .flatten();
        if let Some([primary, secondary]) = adaptive_modes {
            self.primary_mode_counts[usize::from(primary)] += 1;
            self.secondary_mode_counts[usize::from(secondary)] += 1;
        }
        self.observed_report = report_id;
        self.samples += 1;
        let t_ns = received_at
            .checked_duration_since(self.started_at)
            .unwrap_or_default()
            .as_nanos();
        self.write_json(&json!({
            "type": "notification",
            "sequence": self.samples,
            "t_ns": t_ns,
            "address": address,
            "characteristic": characteristic,
            "report_id": report_id.map(|id| format!("0x{id:02x}")),
            "selected_report": format!("0x{:02x}", self.selected_report),
            "label": self.label,
            "adaptive_modes": adaptive_modes,
            "length": bytes.len(),
            "payload": encode_hex(bytes),
        }))
    }

    pub fn record_drop(&mut self, address: &str, count: u64) -> io::Result<()> {
        if address != self.address {
            return Ok(());
        }
        self.write_json(&json!({
            "type": "dropped_notifications",
            "t_ns": self.started_at.elapsed().as_nanos(),
            "count": count,
            "label": self.label,
        }))
    }

    pub fn finish(mut self) -> io::Result<PathBuf> {
        self.write_json(&json!({
            "type": "capture_summary",
            "t_ns": self.started_at.elapsed().as_nanos(),
            "samples": self.samples,
            "common_samples": self.common_samples,
            "native_samples": self.native_samples,
            "primary_mode_counts": self.primary_mode_counts,
            "secondary_mode_counts": self.secondary_mode_counts,
        }))?;
        self.mark("capture_finished")?;
        self.writer.flush()?;
        Ok(self.path.clone())
    }

    fn write_json(&mut self, value: &serde_json::Value) -> io::Result<()> {
        serde_json::to_writer(&mut self.writer, value).map_err(io::Error::other)?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()
    }
}

fn native_adaptive_modes(bytes: &[u8]) -> Option<[u8; 2]> {
    let motion_length = usize::from(*bytes.get(MOTION_LENGTH_OFFSET)?);
    if motion_length != FULL_MOTION_LENGTH || bytes.len() < MOTION_DATA_OFFSET + FULL_MOTION_LENGTH
    {
        return None;
    }
    let body = &bytes[MOTION_DATA_OFFSET + MOTION_HEADER_LENGTH..];
    Some([
        body.first()? & 0b11,
        body.get(SECOND_MODE_BYTE_OFFSET)? & 0b11,
    ])
}

fn capture_directory() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Polymer")
        .join("captures")
}

fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}
