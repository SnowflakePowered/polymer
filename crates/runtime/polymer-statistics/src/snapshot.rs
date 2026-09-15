use std::{
    collections::{HashMap, VecDeque},
    time::Instant,
};

use metrics::Key;
use metrics_util::storage::Summary;

use crate::{
    ROLLING_WINDOW, RecorderHandle, SAMPLE_INTERVAL,
    names::{
        CAPTURE_LATENCY, COMPOSITOR_TICKS, DISPLAY_LATENCY, FRAME_INTERVAL, FRAMES_DROPPED,
        FRAMES_RECEIVED, INPUT_DISPATCH, PRESENTS, REPEATED_PRESENTS,
    },
};

type MetricLabels = Vec<(String, String)>;
type MetricIdentity = (String, MetricLabels);
type CounterSnapshot = HashMap<MetricIdentity, u64>;
type HistogramSnapshot = HashMap<MetricIdentity, Vec<f64>>;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DistributionStatistics {
    pub samples: usize,
    pub average: f64,
    pub minimum: f64,
    pub maximum: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
}

impl DistributionStatistics {
    fn from_values(values: &[f64]) -> Self {
        if values.is_empty() {
            return Self::default();
        }
        let mut summary = Summary::new(0.001, 2_048, 1.0e-9);
        for &value in values {
            summary.add(value);
        }
        Self {
            samples: summary.count(),
            average: summary.sum().unwrap_or_default() / summary.count() as f64,
            minimum: summary.min(),
            maximum: summary.max(),
            p50: summary.quantile(0.50).unwrap_or_default(),
            p95: summary.quantile(0.95).unwrap_or_default(),
            p99: summary.quantile(0.99).unwrap_or_default(),
        }
    }

    pub fn milliseconds(self) -> Self {
        Self {
            samples: self.samples,
            average: self.average * 1_000.0,
            minimum: self.minimum * 1_000.0,
            maximum: self.maximum * 1_000.0,
            p50: self.p50 * 1_000.0,
            p95: self.p95 * 1_000.0,
            p99: self.p99 * 1_000.0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct StatisticsSample {
    pub sampled_at: Instant,
    pub fps: f32,
    pub latency_ms: f32,
    pub frame_interval_ms: f32,
}

#[derive(Clone, Debug, Default)]
pub struct TargetStatistics {
    pub connection: u64,
    pub target: u32,
    pub fps: f64,
    pub dropped: u64,
    pub capture_latency: DistributionStatistics,
    pub frame_interval: DistributionStatistics,
    pub history: VecDeque<StatisticsSample>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PacingStatistics {
    pub presented: f64,
    pub repeated: f64,
    pub clock: f64,
}

#[derive(Clone, Debug, Default)]
pub struct InputStatistics {
    pub providers: HashMap<String, DistributionStatistics>,
    aggregate: DistributionStatistics,
}

impl InputStatistics {
    pub fn combined(&self) -> DistributionStatistics {
        self.aggregate
    }
}

#[derive(Clone, Debug)]
pub struct StatisticsSnapshot {
    pub sampled_at: Instant,
    pub targets: Vec<TargetStatistics>,
    pub pacing: PacingStatistics,
    pub input: InputStatistics,
    pub display_latency: HashMap<String, DistributionStatistics>,
    pub dropped_frames: u64,
}

impl Default for StatisticsSnapshot {
    fn default() -> Self {
        Self {
            sampled_at: Instant::now(),
            targets: Vec::new(),
            pacing: PacingStatistics::default(),
            input: InputStatistics::default(),
            display_latency: HashMap::new(),
            dropped_frames: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct TargetKey {
    connection: u64,
    target: u32,
}

#[derive(Debug)]
pub struct LiveStatistics {
    recorder: RecorderHandle,
    last_sample: Option<Instant>,
    targets: HashMap<TargetKey, TargetStatistics>,
    dropped_frames: u64,
    snapshot: StatisticsSnapshot,
}

impl LiveStatistics {
    pub fn new(recorder: RecorderHandle) -> Self {
        Self {
            recorder,
            last_sample: None,
            targets: HashMap::new(),
            dropped_frames: 0,
            snapshot: StatisticsSnapshot::default(),
        }
    }

    pub fn sample(&mut self) -> &StatisticsSnapshot {
        self.sample_at(Instant::now())
    }

    pub fn sample_at(&mut self, now: Instant) -> &StatisticsSnapshot {
        if self
            .last_sample
            .is_some_and(|last| now.saturating_duration_since(last) < SAMPLE_INTERVAL)
        {
            return &self.snapshot;
        }
        let drained = self.recorder.drain_at(now);
        let elapsed = self
            .last_sample
            .map_or(SAMPLE_INTERVAL, |last| now.saturating_duration_since(last))
            .as_secs_f64()
            .max(f64::EPSILON);
        self.last_sample = Some(now);

        let mut counters = CounterSnapshot::new();
        for (key, value) in drained.counters {
            counters.insert(key_parts(&key), value);
        }
        let mut histograms = HistogramSnapshot::new();
        for (key, values) in drained.histograms {
            histograms.insert(key_parts(&key), values);
        }

        for ((name, labels), received) in &counters {
            if name != FRAMES_RECEIVED {
                continue;
            }
            let Some(connection) = label(labels, "connection").and_then(|value| value.parse().ok())
            else {
                continue;
            };
            let Some(target) = label(labels, "target").and_then(|value| value.parse().ok()) else {
                continue;
            };
            let key = TargetKey { connection, target };
            let dropped = counter_for(&counters, FRAMES_DROPPED, connection);
            self.dropped_frames += dropped;
            let frame_interval =
                distribution_for(&histograms, FRAME_INTERVAL, connection).milliseconds();
            let capture_latency =
                distribution_for(&histograms, CAPTURE_LATENCY, connection).milliseconds();
            let statistics = self.targets.entry(key).or_default();
            statistics.connection = connection;
            statistics.target = target;
            statistics.fps = *received as f64 / elapsed;
            statistics.dropped += dropped;
            statistics.frame_interval = frame_interval;
            statistics.capture_latency = capture_latency;
            statistics.history.push_back(StatisticsSample {
                sampled_at: now,
                fps: statistics.fps as f32,
                latency_ms: capture_latency.average as f32,
                frame_interval_ms: frame_interval.average as f32,
            });
            while statistics.history.front().is_some_and(|sample| {
                now.saturating_duration_since(sample.sampled_at) > ROLLING_WINDOW
            }) {
                statistics.history.pop_front();
            }
        }

        let mut targets = self.targets.values().cloned().collect::<Vec<_>>();
        targets.sort_by_key(|statistics| (statistics.target, statistics.connection));

        let pacing = PacingStatistics {
            presented: sum_counter(&counters, PRESENTS) as f64 / elapsed,
            repeated: sum_counter(&counters, REPEATED_PRESENTS) as f64 / elapsed,
            clock: sum_counter(&counters, COMPOSITOR_TICKS) as f64 / elapsed,
        };

        let mut input = InputStatistics::default();
        let mut input_values = Vec::new();
        let mut display_latency = HashMap::new();
        for ((name, labels), values) in histograms {
            match name.as_str() {
                INPUT_DISPATCH => {
                    input_values.extend_from_slice(&values);
                    if let Some(provider) = label(&labels, "provider") {
                        input.providers.insert(
                            provider.to_owned(),
                            DistributionStatistics::from_values(&values).milliseconds(),
                        );
                    }
                }
                DISPLAY_LATENCY => {
                    if let Some(backend) = label(&labels, "backend") {
                        display_latency.insert(
                            backend.to_owned(),
                            DistributionStatistics::from_values(&values).milliseconds(),
                        );
                    }
                }
                _ => {}
            }
        }
        input.aggregate = DistributionStatistics::from_values(&input_values).milliseconds();

        self.snapshot = StatisticsSnapshot {
            sampled_at: drained.sampled_at,
            targets,
            pacing,
            input,
            display_latency,
            dropped_frames: self.dropped_frames,
        };
        &self.snapshot
    }

    pub fn remove_connection(&mut self, connection: u64) {
        crate::remove_connection(&self.recorder, connection);
        self.targets.retain(|key, _| key.connection != connection);
        self.snapshot
            .targets
            .retain(|statistics| statistics.connection != connection);
    }

    pub fn snapshot(&self) -> &StatisticsSnapshot {
        &self.snapshot
    }

    pub fn recorder(&self) -> &RecorderHandle {
        &self.recorder
    }
}

fn key_parts(key: &Key) -> MetricIdentity {
    (
        key.name().to_owned(),
        key.labels()
            .map(|label| (label.key().to_owned(), label.value().to_owned()))
            .collect(),
    )
}

fn label<'a>(labels: &'a MetricLabels, name: &str) -> Option<&'a str> {
    labels
        .iter()
        .find_map(|(key, value)| (key == name).then_some(value.as_str()))
}

fn counter_for(counters: &CounterSnapshot, name: &str, connection: u64) -> u64 {
    let connection = connection.to_string();
    counters
        .iter()
        .filter(|((candidate, labels), _)| {
            candidate == name && label(labels, "connection") == Some(connection.as_str())
        })
        .map(|(_, value)| *value)
        .sum()
}

fn sum_counter(counters: &CounterSnapshot, name: &str) -> u64 {
    counters
        .iter()
        .filter(|((candidate, _), _)| candidate == name)
        .map(|(_, value)| *value)
        .sum()
}

fn distribution_for(
    histograms: &HistogramSnapshot,
    name: &str,
    connection: u64,
) -> DistributionStatistics {
    let connection = connection.to_string();
    let values = histograms
        .iter()
        .filter(|((candidate, labels), _)| {
            candidate == name && label(labels, "connection") == Some(connection.as_str())
        })
        .flat_map(|(_, values)| values.iter().copied())
        .collect::<Vec<_>>();
    DistributionStatistics::from_values(&values)
}
