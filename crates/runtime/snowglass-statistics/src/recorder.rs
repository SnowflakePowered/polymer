use std::{
    sync::{Arc, atomic::Ordering},
    time::Instant,
};

use metrics::{
    Counter, Gauge, Histogram, Key, KeyName, Metadata, Recorder, SetRecorderError, SharedString,
    Unit,
};
use metrics_util::registry::{AtomicStorage, Registry};

/// Global-recorder implementation backed by atomic metric handles.
#[derive(Clone, Debug)]
pub struct LiveRecorder {
    registry: Arc<Registry<Key, AtomicStorage>>,
}

impl LiveRecorder {
    pub fn new() -> (Self, RecorderHandle) {
        let registry = Arc::new(Registry::atomic());
        (
            Self {
                registry: Arc::clone(&registry),
            },
            RecorderHandle { registry },
        )
    }
}

impl Recorder for LiveRecorder {
    fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

    fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

    fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

    fn register_counter(&self, key: &Key, _: &Metadata<'_>) -> Counter {
        let key = key.to_retained();
        self.registry
            .get_or_create_counter(&key, |counter| Counter::from_arc(counter.clone()))
    }

    fn register_gauge(&self, key: &Key, _: &Metadata<'_>) -> Gauge {
        let key = key.to_retained();
        self.registry
            .get_or_create_gauge(&key, |gauge| Gauge::from_arc(gauge.clone()))
    }

    fn register_histogram(&self, key: &Key, _: &Metadata<'_>) -> Histogram {
        let key = key.to_retained();
        self.registry
            .get_or_create_histogram(&key, |histogram| Histogram::from_arc(histogram.clone()))
    }
}

#[derive(Debug)]
pub(crate) struct DrainedMetrics {
    pub sampled_at: Instant,
    pub counters: Vec<(Key, u64)>,
    pub histograms: Vec<(Key, Vec<f64>)>,
}

/// Read and lifecycle handle retained by the viewer after recorder installation.
#[derive(Clone, Debug)]
pub struct RecorderHandle {
    registry: Arc<Registry<Key, AtomicStorage>>,
}

impl RecorderHandle {
    pub(crate) fn drain_at(&self, sampled_at: Instant) -> DrainedMetrics {
        let mut counters = Vec::new();
        self.registry.visit_counters(|key, counter| {
            counters.push((key.clone(), counter.swap(0, Ordering::AcqRel)));
        });

        let mut histograms = Vec::new();
        self.registry.visit_histograms(|key, histogram| {
            let mut values = Vec::new();
            histogram.clear_with(|slice| values.extend_from_slice(slice));
            histograms.push((key.clone(), values));
        });

        DrainedMetrics {
            sampled_at,
            counters,
            histograms,
        }
    }

    pub fn remove_label(&self, label: &str, value: &str) {
        let matches = |key: &Key| {
            key.labels()
                .any(|candidate| candidate.key() == label && candidate.value() == value)
        };
        self.registry.retain_counters(|key, _| !matches(key));
        self.registry.retain_gauges(|key, _| !matches(key));
        self.registry.retain_histograms(|key, _| !matches(key));
    }
}

pub fn install() -> Result<RecorderHandle, SetRecorderError<LiveRecorder>> {
    let (recorder, handle) = LiveRecorder::new();
    metrics::set_global_recorder(recorder)?;
    Ok(handle)
}
