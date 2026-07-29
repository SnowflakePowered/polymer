//! Process-local live metrics and rolling statistics for Snowglass viewers.

mod emit;
mod recorder;
mod snapshot;

pub use emit::{
    FrameMetrics, InputMetrics, PresentationMetrics, display_latency, remove_connection,
};
pub use recorder::{LiveRecorder, RecorderHandle, install};
pub use snapshot::{
    DistributionStatistics, InputStatistics, LiveStatistics, PacingStatistics, StatisticsSample,
    StatisticsSnapshot, TargetStatistics,
};

pub const SAMPLE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);
pub const ROLLING_WINDOW: std::time::Duration = std::time::Duration::from_secs(120);

mod names {
    pub const FRAMES_RECEIVED: &str = "snowglass_frames_received_total";
    pub const FRAMES_DROPPED: &str = "snowglass_frames_dropped_total";
    pub const FRAME_INTERVAL: &str = "snowglass_frame_interval_seconds";
    pub const CAPTURE_LATENCY: &str = "snowglass_capture_latency_seconds";
    pub const DISPLAY_LATENCY: &str = "snowglass_display_latency_seconds";
    pub const PRESENTS: &str = "snowglass_presents_total";
    pub const REPEATED_PRESENTS: &str = "snowglass_repeated_presents_total";
    pub const COMPOSITOR_TICKS: &str = "snowglass_compositor_ticks_total";
    pub const INPUT_DISPATCH: &str = "snowglass_input_dispatch_seconds";
}

mod labels {
    pub const CONNECTION: &str = "connection";
    pub const SURFACE: &str = "surface";
    pub const TARGET: &str = "target";
    pub const BACKEND: &str = "backend";
    pub const PROVIDER: &str = "provider";
}
