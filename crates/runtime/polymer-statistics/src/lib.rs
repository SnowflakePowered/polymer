//! Process-local live metrics and rolling statistics for Polymer viewers.

mod emit;
mod recorder;
mod snapshot;

pub use emit::{
    FrameMetrics, InputMetrics, PresentationMetrics, capture_latency, display_latency,
    remove_connection,
};
pub use recorder::{LiveRecorder, RecorderHandle, install};
pub use snapshot::{
    DistributionStatistics, InputStatistics, LiveStatistics, PacingStatistics, StatisticsSample,
    StatisticsSnapshot, TargetStatistics,
};

pub const SAMPLE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);
pub const ROLLING_WINDOW: std::time::Duration = std::time::Duration::from_secs(120);

mod names {
    pub const FRAMES_RECEIVED: &str = "polymer_frames_received_total";
    pub const FRAMES_DROPPED: &str = "polymer_frames_dropped_total";
    pub const FRAME_INTERVAL: &str = "polymer_frame_interval_seconds";
    pub const CAPTURE_LATENCY: &str = "polymer_capture_latency_seconds";
    pub const DISPLAY_LATENCY: &str = "polymer_display_latency_seconds";
    pub const PRESENTS: &str = "polymer_presents_total";
    pub const REPEATED_PRESENTS: &str = "polymer_repeated_presents_total";
    pub const COMPOSITOR_TICKS: &str = "polymer_compositor_ticks_total";
    pub const INPUT_DISPATCH: &str = "polymer_input_dispatch_seconds";
}

mod labels {
    pub const CONNECTION: &str = "connection";
    pub const SURFACE: &str = "surface";
    pub const TARGET: &str = "target";
    pub const BACKEND: &str = "backend";
    pub const PROVIDER: &str = "provider";
}
