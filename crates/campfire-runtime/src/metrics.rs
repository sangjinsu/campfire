use serde::Serialize;
use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::time::Instant;

const RECENT_WINDOW: Duration = Duration::from_secs(60);
const RECENT_CAPACITY: usize = 512;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct LatencySnapshot {
    pub sample_count: u64,
    pub p95_micros: Option<u64>,
    pub p99_micros: Option<u64>,
}
#[derive(Clone, Debug, Serialize)]
pub struct RecentMetricsSnapshot {
    pub window_ms: u64,
    pub tick_delay: LatencySnapshot,
    pub tick_duration: LatencySnapshot,
    pub input_latency: LatencySnapshot,
    pub input_duration: LatencySnapshot,
    pub command_duration: LatencySnapshot,
}
impl Default for RecentMetricsSnapshot {
    fn default() -> Self {
        Self {
            window_ms: 60000,
            tick_delay: LatencySnapshot::default(),
            tick_duration: LatencySnapshot::default(),
            input_latency: LatencySnapshot::default(),
            input_duration: LatencySnapshot::default(),
            command_duration: LatencySnapshot::default(),
        }
    }
}
#[derive(Default)]
pub(crate) struct RecentLatency {
    samples: Mutex<VecDeque<(Instant, u64)>>,
}
impl RecentLatency {
    pub fn record(&self, maximum: &AtomicU64, micros: u64) {
        max(maximum, micros);
        let mut samples = self
            .samples
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let now = Instant::now();
        samples.retain(|(at, _)| now.saturating_duration_since(*at) < RECENT_WINDOW);
        if samples.len() == RECENT_CAPACITY {
            samples.pop_front();
        }
        samples.push_back((now, micros));
    }
    fn snapshot(&self, now: Instant) -> LatencySnapshot {
        let mut values: Vec<u64> = {
            let mut samples = self
                .samples
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            samples.retain(|(at, _)| now.saturating_duration_since(*at) < RECENT_WINDOW);
            samples.iter().map(|(_, value)| *value).collect()
        };
        // Sorting and percentile calculation never hold the recorder lock.
        values.sort_unstable();
        let percentile = |percent: usize| {
            if values.is_empty() {
                None
            } else {
                Some(values[(values.len() * percent).div_ceil(100) - 1])
            }
        };
        LatencySnapshot {
            sample_count: values.len() as u64,
            p95_micros: percentile(95),
            p99_micros: percentile(99),
        }
    }
}
#[derive(Default)]
pub(crate) struct RecentMetrics {
    pub tick_delay: RecentLatency,
    pub tick_duration: RecentLatency,
    pub input_latency: RecentLatency,
    pub input_duration: RecentLatency,
    pub command_duration: RecentLatency,
}
impl RecentMetrics {
    fn snapshot(&self) -> RecentMetricsSnapshot {
        let now = Instant::now();
        RecentMetricsSnapshot {
            window_ms: RECENT_WINDOW.as_millis() as u64,
            tick_delay: self.tick_delay.snapshot(now),
            tick_duration: self.tick_duration.snapshot(now),
            input_latency: self.input_latency.snapshot(now),
            input_duration: self.input_duration.snapshot(now),
            command_duration: self.command_duration.snapshot(now),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct MetricsSnapshot {
    pub active_sessions: u64,
    pub active_connections: u64,
    pub unauthenticated_connections: u64,
    pub active_rooms: u64,
    pub rate_keys: u64,
    pub guests_issued: u64,
    pub admission_rejections: u64,
    pub abusive_frames: u64,
    pub abusive_inputs: u64,
    pub shared_queue_overloads: u64,
    pub reliable_queue_disconnects: u64,
    pub write_deadline_disconnects: u64,
    pub idle_disconnects: u64,
    pub room_panics: u64,
    pub ticks: u64,
    pub inputs_processed: u64,
    pub snapshots: u64,
    pub commands_processed: u64,
    pub commands_failed: u64,
    pub command_replays: u64,
    pub max_command_duration_micros: u64,
    pub recent: RecentMetricsSnapshot,
    pub max_input_queue_depth: u64,
    pub max_reliable_queue_depth: u64,
    pub max_tick_delay_micros: u64,
    pub max_tick_duration_micros: u64,
    pub max_input_latency_micros: u64,
    pub max_input_duration_micros: u64,
}
#[derive(Default)]
pub(crate) struct Metrics {
    pub active_sessions: AtomicU64,
    pub active_rooms: AtomicU64,
    pub rate_keys: AtomicU64,
    pub guests_issued: AtomicU64,
    pub admission_rejections: AtomicU64,
    pub abusive_frames: AtomicU64,
    pub abusive_inputs: AtomicU64,
    pub shared_queue_overloads: AtomicU64,
    pub reliable_queue_disconnects: AtomicU64,
    pub write_deadline_disconnects: AtomicU64,
    pub idle_disconnects: AtomicU64,
    pub room_panics: AtomicU64,
    pub ticks: AtomicU64,
    pub inputs_processed: AtomicU64,
    pub snapshots: AtomicU64,
    pub commands_processed: AtomicU64,
    pub commands_failed: AtomicU64,
    pub command_replays: AtomicU64,
    pub max_command_duration_micros: AtomicU64,
    pub recent: RecentMetrics,
    pub max_input_queue_depth: AtomicU64,
    pub max_reliable_queue_depth: AtomicU64,
    pub max_tick_delay_micros: AtomicU64,
    pub max_tick_duration_micros: AtomicU64,
    pub max_input_latency_micros: AtomicU64,
    pub max_input_duration_micros: AtomicU64,
}
impl Metrics {
    pub fn snapshot(&self) -> MetricsSnapshot {
        macro_rules! get {
            ($field:ident) => {
                self.$field.load(Ordering::Relaxed)
            };
        }
        MetricsSnapshot {
            active_sessions: get!(active_sessions),
            active_rooms: get!(active_rooms),
            rate_keys: get!(rate_keys),
            guests_issued: get!(guests_issued),
            admission_rejections: get!(admission_rejections),
            abusive_frames: get!(abusive_frames),
            abusive_inputs: get!(abusive_inputs),
            shared_queue_overloads: get!(shared_queue_overloads),
            reliable_queue_disconnects: get!(reliable_queue_disconnects),
            write_deadline_disconnects: get!(write_deadline_disconnects),
            idle_disconnects: get!(idle_disconnects),
            room_panics: get!(room_panics),
            ticks: get!(ticks),
            inputs_processed: get!(inputs_processed),
            snapshots: get!(snapshots),
            commands_processed: get!(commands_processed),
            commands_failed: get!(commands_failed),
            command_replays: get!(command_replays),
            max_command_duration_micros: get!(max_command_duration_micros),
            recent: self.recent.snapshot(),
            max_input_queue_depth: get!(max_input_queue_depth),
            max_reliable_queue_depth: get!(max_reliable_queue_depth),
            max_tick_delay_micros: get!(max_tick_delay_micros),
            max_tick_duration_micros: get!(max_tick_duration_micros),
            max_input_latency_micros: get!(max_input_latency_micros),
            max_input_duration_micros: get!(max_input_duration_micros),
            ..MetricsSnapshot::default()
        }
    }
}
pub(crate) fn inc(value: &AtomicU64) {
    value.fetch_add(1, Ordering::Relaxed);
}
pub(crate) fn max(value: &AtomicU64, sample: u64) {
    value.fetch_max(sample, Ordering::Relaxed);
}
