//! Lossless Wii Remote HID forwarding for emulator-native backends.

use crate::decoder::wiimote::ReportMetadata;
use snowglass_bluetooth::{BluetoothManager, ManagerEvent, SessionId};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::windows::named_pipe::{NamedPipeServer, PipeMode, ServerOptions},
    task::JoinHandle,
};

const DOLPHIN_INPUT_REPORT_SIZE: usize = 22;
const SPEAKER_DATA_REPORT: u8 = 0x18;
const RUMBLE_REPORT: u8 = 0x10;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawWiimotePipeConfig {
    pub source_id: String,
    pub session_id: SessionId,
    pub port: u8,
    pub product_id: u16,
    pub pipe_name: String,
}

pub struct RawWiimoteSession {
    manager: BluetoothManager,
    sessions: Vec<SessionId>,
    capture_requested: Arc<AtomicBool>,
    tasks: Vec<JoinHandle<()>>,
}

impl RawWiimoteSession {
    pub async fn start(
        manager: BluetoothManager,
        pipes: Vec<RawWiimotePipeConfig>,
        failed: Arc<dyn Fn(String) + Send + Sync>,
    ) -> Result<Self, String> {
        let capture_requested = Arc::new(AtomicBool::new(false));
        let servers = pipes
            .into_iter()
            .map(|pipe| {
                let server = ServerOptions::new()
                    .pipe_mode(PipeMode::Message)
                    .create(&pipe.pipe_name)
                    .map_err(|error| format!("could not create {}: {error}", pipe.pipe_name))?;
                Ok((pipe, server))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let mut sessions = Vec::with_capacity(servers.len());
        let mut tasks = Vec::with_capacity(servers.len());
        for (pipe, server) in servers {
            if let Err(error) = manager.set_external_ownership(pipe.session_id, true).await {
                for session_id in &sessions {
                    let _ = manager.set_external_ownership(*session_id, false).await;
                }
                return Err(error.to_string());
            }
            sessions.push(pipe.session_id);
            tasks.push(tokio::spawn(Self::serve(
                manager.clone(),
                pipe,
                server,
                Arc::clone(&capture_requested),
                Arc::clone(&failed),
            )));
        }
        Ok(Self {
            manager,
            sessions,
            capture_requested,
            tasks,
        })
    }

    pub fn set_capture(&self, capture: bool) {
        self.capture_requested.store(capture, Ordering::Release);
    }

    async fn serve(
        manager: BluetoothManager,
        pipe: RawWiimotePipeConfig,
        server: NamedPipeServer,
        capture_requested: Arc<AtomicBool>,
        failed: Arc<dyn Fn(String) + Send + Sync>,
    ) {
        if let Err(error) = server.connect().await {
            tracing::error!(port = pipe.port, %error, "Dolphin Wii pipe connection failed");
            tokio::time::sleep(Duration::from_secs(5)).await;
            let _ = manager.set_external_ownership(pipe.session_id, false).await;
            failed(pipe.source_id);
            return;
        }
        let mut events = manager.events();
        let (mut reader, mut writer) = tokio::io::split(server);
        let mut output = [0u8; DOLPHIN_INPUT_REPORT_SIZE];
        let mut capturing = false;
        let mut clear_since = None;
        loop {
            tokio::select! {
                event = events.recv() => match event {
                    Ok(ManagerEvent::RawReport { session_id, bytes, .. })
                        if session_id == pipe.session_id =>
                    {
                        let mut report = [0u8; DOLPHIN_INPUT_REPORT_SIZE];
                        let length = bytes.len().min(report.len());
                        report[..length].copy_from_slice(&bytes[..length]);
                        let requested = capture_requested.load(Ordering::Acquire);
                        if requested {
                            capturing = true;
                            clear_since = None;
                        } else if capturing {
                            let clear = ReportMetadata::inspect(&report[..length])
                                .is_some_and(|metadata| {
                                    metadata.physical_buttons_clear(&report[..length])
                                });
                            if clear {
                                let since = clear_since.get_or_insert_with(std::time::Instant::now);
                                if since.elapsed() >= Duration::from_millis(50) {
                                    capturing = false;
                                    clear_since = None;
                                }
                            } else {
                                clear_since = None;
                            }
                        }
                        Self::filter_input(&mut report[..length], capturing);
                        if let Err(error) = writer.write_all(&report).await {
                            tracing::error!(port = pipe.port, %error, "Dolphin Wii pipe write failed");
                            break;
                        }
                    }
                    Ok(ManagerEvent::Disconnected { session_id, .. })
                        if session_id == pipe.session_id => break,
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::error!(port = pipe.port, skipped, "raw Wii report stream overflowed");
                        break;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                },
                read = reader.read(&mut output) => match read {
                    Ok(0) => break,
                    Ok(length) => {
                        let command = Self::filter_output(
                            &output[..length],
                            capturing,
                        );
                        if let Err(error) = manager.send_raw(pipe.session_id, &command).await {
                            tracing::error!(port = pipe.port, %error, "Dolphin Wii command failed");
                            break;
                        }
                    }
                    Err(error) => {
                        tracing::error!(port = pipe.port, %error, "Dolphin Wii pipe read failed");
                        break;
                    }
                },
            }
        }
        // Dolphin deliberately closes every Real Wiimote during an atomic
        // topology rebind. The replacement session aborts this task inside
        // the grace period; only a persistent closure becomes a fallback.
        tokio::time::sleep(Duration::from_secs(5)).await;
        let _ = manager.set_external_ownership(pipe.session_id, false).await;
        failed(pipe.source_id);
    }

    fn filter_input(report: &mut [u8], capture: bool) {
        if let Some(metadata) = ReportMetadata::inspect(report) {
            metadata.filter_for_dolphin(report, capture);
        }
    }

    fn filter_output(report: &[u8], capture: bool) -> Vec<u8> {
        if capture && report.first() == Some(&SPEAKER_DATA_REPORT) {
            return vec![
                RUMBLE_REPORT,
                report.get(1).copied().unwrap_or_default() & 1,
            ];
        }
        report.to_vec()
    }
}

impl Drop for RawWiimoteSession {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
        for session_id in self.sessions.drain(..) {
            let manager = self.manager.clone();
            tokio::spawn(async move {
                let _ = tokio::time::timeout(
                    Duration::from_secs(2),
                    manager.set_external_ownership(session_id, false),
                )
                .await;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RawWiimoteSession;

    #[test]
    fn capture_mask_preserves_accelerometer_low_bits_and_hides_home() {
        let mut report = [0x37, 0xff, 0xff, 1, 2, 3, 4, 5, 6];
        RawWiimoteSession::filter_input(&mut report, true);
        assert_eq!(report, [0x37, 0xe0, 0x60, 1, 2, 3, 4, 5, 6]);

        let mut report = [0x31, 0x08, 0x80];
        RawWiimoteSession::filter_input(&mut report, false);
        assert_eq!(report, [0x31, 0x08, 0x00]);
    }

    #[test]
    fn captured_speaker_data_retains_the_rumble_bit() {
        assert_eq!(
            RawWiimoteSession::filter_output(&[0x18, 0x01, 0xaa, 0xbb], true),
            [0x10, 0x01]
        );
    }
}
