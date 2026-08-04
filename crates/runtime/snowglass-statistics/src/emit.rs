use std::time::Duration;

use metrics::{Counter, Histogram, counter, histogram};

use crate::{RecorderHandle, labels, names};

#[derive(Clone, Copy, Debug)]
struct StreamSequence {
    frame_number: u64,
    timestamp_qpc: u64,
}

/// Cached handles and sequence state for one captured surface.
#[derive(Clone, Debug)]
pub struct FrameMetrics {
    connection: u64,
    received: Counter,
    dropped: Counter,
    interval: Histogram,
    capture_latency: Histogram,
    previous: Option<StreamSequence>,
    dropped_total: u64,
}

impl FrameMetrics {
    pub fn new(connection: u64, surface: u64, target: u32) -> Self {
        let connection_label = connection.to_string();
        let surface_label = surface.to_string();
        let target_label = target.to_string();
        Self {
            connection,
            received: counter!(
                names::FRAMES_RECEIVED,
                labels::CONNECTION => connection_label.clone(),
                labels::SURFACE => surface_label.clone(),
                labels::TARGET => target_label,
            ),
            dropped: counter!(
                names::FRAMES_DROPPED,
                labels::CONNECTION => connection_label.clone(),
                labels::SURFACE => surface_label.clone(),
            ),
            interval: histogram!(
                names::FRAME_INTERVAL,
                labels::CONNECTION => connection_label.clone(),
                labels::SURFACE => surface_label.clone(),
            ),
            capture_latency: histogram!(
                names::CAPTURE_LATENCY,
                labels::CONNECTION => connection_label,
                labels::SURFACE => surface_label,
            ),
            previous: None,
            dropped_total: 0,
        }
    }

    /// Records a received producer frame and returns newly detected frame gaps.
    pub fn observe(
        &mut self,
        frame_number: u64,
        timestamp_qpc: u64,
        counter_frequency: f64,
        capture_latency: Duration,
    ) -> u64 {
        self.received.increment(1);
        self.capture_latency.record(capture_latency.as_secs_f64());

        let mut newly_dropped = 0;
        if let Some(previous) = self.previous
            && frame_number > previous.frame_number
        {
            let frame_delta = frame_number - previous.frame_number;
            if let Some(ticks) = timestamp_qpc.checked_sub(previous.timestamp_qpc) {
                self.interval
                    .record(ticks as f64 / frame_delta as f64 / counter_frequency.max(1.0));
            }
            newly_dropped = frame_delta - 1;
            if newly_dropped > 0 {
                self.dropped.increment(newly_dropped);
                self.dropped_total += newly_dropped;
            }
        }
        self.previous = Some(StreamSequence {
            frame_number,
            timestamp_qpc,
        });
        newly_dropped
    }

    pub const fn connection(&self) -> u64 {
        self.connection
    }

    pub const fn dropped(&self) -> u64 {
        self.dropped_total
    }
}

#[derive(Clone, Debug)]
pub struct PresentationMetrics {
    presents: Counter,
    repeated: Counter,
    ticks: Counter,
}

impl PresentationMetrics {
    pub fn new(backend: &'static str) -> Self {
        Self {
            presents: counter!(names::PRESENTS, labels::BACKEND => backend),
            repeated: counter!(names::REPEATED_PRESENTS, labels::BACKEND => backend),
            ticks: counter!(names::COMPOSITOR_TICKS),
        }
    }

    pub fn record_present(&self, carried_new_frame: bool) {
        self.presents.increment(1);
        if !carried_new_frame {
            self.repeated.increment(1);
        }
    }

    pub fn record_clock(&self) {
        self.ticks.increment(1);
    }
}

#[derive(Clone, Debug)]
pub struct InputMetrics {
    dispatch: Histogram,
}

impl InputMetrics {
    pub fn new(provider: &'static str) -> Self {
        Self {
            dispatch: histogram!(names::INPUT_DISPATCH, labels::PROVIDER => provider),
        }
    }

    pub fn record_dispatch(&self, latency: Duration) {
        self.dispatch.record(latency.as_secs_f64());
    }
}

pub fn display_latency(backend: &'static str, latency: Duration) {
    histogram!(names::DISPLAY_LATENCY, labels::BACKEND => backend).record(latency.as_secs_f64());
}

pub fn remove_connection(recorder: &RecorderHandle, connection: u64) {
    recorder.remove_label(labels::CONNECTION, &connection.to_string());
}
