use super::{
    Output,
    motion::{MotionCalibration, MotionState, Tracker},
};
use crate::{
    ReportReceiver, Result,
    transport::{HidControl, HidReportType},
};
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast::error::RecvError, oneshot};

pub(super) struct Runtime {
    output: Arc<Mutex<Output>>,
    motion: MotionState,
    control: Arc<dyn HidControl>,
    tracker: Tracker,
}

impl Runtime {
    pub fn new(
        output: Arc<Mutex<Output>>,
        motion: MotionState,
        control: Arc<dyn HidControl>,
        calibration: MotionCalibration,
    ) -> Self {
        Self {
            output,
            motion,
            control,
            tracker: Tracker::new(calibration),
        }
    }

    pub async fn run(mut self, mut reports: ReportReceiver, mut stopped: oneshot::Receiver<()>) {
        loop {
            let received = tokio::select! {
                _ = &mut stopped => break,
                report = reports.recv() => report,
            };
            match received {
                Ok(report) => {
                    if let Err(error) = self.update(&report).await {
                        tracing::warn!(%error, "DS3 gyro trim update failed; reconnect to retry");
                        break;
                    }
                }
                Err(RecvError::Lagged(_)) => self.tracker = Tracker::new(self.tracker.calibration),
                Err(RecvError::Closed) => break,
            }
        }
    }

    async fn update(&mut self, report: &[u8]) -> Result<()> {
        if report.len() != 49 || report[0] != 1 || report[1] == 0xff {
            return Ok(());
        }
        let raw = u16::from_be_bytes([report[47], report[48]]);
        let Some(next) = self.tracker.observe(raw) else {
            return Ok(());
        };
        let mut current = self.output.lock().await;
        if next.cal_byte != self.tracker.calibration.cal_byte {
            let mut candidate = current.clone();
            candidate.motion = Some(next);
            self.control
                .set_report(HidReportType::Output, &candidate.report()?)
                .await?;
            tracing::info!(cal_byte = next.cal_byte, "DS3 gyro hardware trim updated");
        }
        current.motion = Some(next);
        self.tracker.commit(next);
        self.motion.publish(next);
        Ok(())
    }
}
