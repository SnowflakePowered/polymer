use std::{
    fs::{File, OpenOptions},
    io::Write,
    sync::{Mutex, OnceLock},
    time::Instant,
};

pub const TRACE_PATH_VARIABLE: &str = "SNOWGLASS_BT_TRACE";

pub const COMMAND: u8 = 0x01;
pub const ACL: u8 = 0x02;
pub const EVENT: u8 = 0x04;

#[derive(Clone, Copy)]
pub enum Direction {
    HostToController,
    ControllerToHost,
}

struct Recorder {
    file: Mutex<File>,
    started: Instant,
}

fn recorder() -> Option<&'static Recorder> {
    static RECORDER: OnceLock<Option<Recorder>> = OnceLock::new();
    RECORDER
        .get_or_init(|| {
            let path = std::env::var_os(TRACE_PATH_VARIABLE)?;
            let file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&path)
                .ok()?;
            eprintln!(
                "Recording raw H4 Bluetooth trace to {}",
                path.to_string_lossy()
            );
            Some(Recorder {
                file: Mutex::new(file),
                started: Instant::now(),
            })
        })
        .as_ref()
}

pub fn enabled() -> bool {
    recorder().is_some()
}

pub fn record(direction: Direction, packet_type: u8, payload: &[u8]) {
    let Some(recorder) = recorder() else {
        return;
    };
    let mut line = String::with_capacity(8 + 3 * payload.len());
    line.push(match direction {
        Direction::HostToController => 'O',
        Direction::ControllerToHost => 'I',
    });
    line.push_str(&format!(
        " {:9.3} {packet_type:02X}",
        recorder.started.elapsed().as_secs_f64()
    ));
    let redacted = redact(packet_type, payload);
    for byte in redacted.as_deref().unwrap_or(payload) {
        line.push_str(&format!(" {byte:02X}"));
    }
    if let Ok(mut file) = recorder.file.lock() {
        let _ = writeln!(file, "{line}");
        let _ = file.flush();
    }
}

fn redact(packet_type: u8, payload: &[u8]) -> Option<Vec<u8>> {
    let key_offset = match packet_type {
        COMMAND if payload.len() >= 25 && payload[0] == 0x0b && payload[1] == 0x04 => 9,
        EVENT if payload.len() >= 24 && payload[0] == 0x18 => 8,
        _ => return None,
    };
    let mut redacted = payload.to_vec();
    redacted[key_offset..key_offset + 16].fill(0);
    Some(redacted)
}
