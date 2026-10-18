use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};

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
    pub room_panics: u64,
    pub ticks: u64,
    pub inputs_processed: u64,
    pub snapshots: u64,
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
    pub room_panics: AtomicU64,
    pub ticks: AtomicU64,
    pub inputs_processed: AtomicU64,
    pub snapshots: AtomicU64,
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
            room_panics: get!(room_panics),
            ticks: get!(ticks),
            inputs_processed: get!(inputs_processed),
            snapshots: get!(snapshots),
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
