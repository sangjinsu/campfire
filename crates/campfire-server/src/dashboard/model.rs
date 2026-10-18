use campfire_runtime::{MetricsSnapshot, RoomObservation, RuntimeObservation};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use tokio::time::Instant;

pub(super) const STALE_MS: u64 = 3_000;
pub(super) const HISTORY_MS: u64 = 15 * 60 * 1_000;
pub(super) const HISTORY_LIMIT: usize = 900;

#[derive(Clone, Default, Serialize)]
pub(super) struct RoomTotals {
    pub total: usize,
    pub public: usize,
    pub private: usize,
    pub connected_members: usize,
    pub reserved_members: usize,
}
impl RoomTotals {
    fn add(&mut self, room: &RoomObservation) {
        self.total += 1;
        self.public += usize::from(room.public);
        self.private += usize::from(!room.public);
        self.connected_members += room.connected_members;
        self.reserved_members += room.reserved_members;
    }
}
#[derive(Clone, Serialize)]
pub(super) struct GameTotals {
    pub game_id: String,
    #[serde(flatten)]
    pub totals: RoomTotals,
}

// This explicit projection prevents future runtime fields from silently entering the API.
macro_rules! metrics {
    ($($name:ident),* $(,)?) => {
        #[derive(Clone, Default, Serialize)]
        pub(super) struct Metrics { $(pub $name: u64,)* }
        impl From<&MetricsSnapshot> for Metrics {
            fn from(source: &MetricsSnapshot) -> Self { Self { $($name: source.$name,)* } }
        }
    }
}
metrics!(
    active_sessions,
    active_connections,
    unauthenticated_connections,
    active_rooms,
    rate_keys,
    guests_issued,
    admission_rejections,
    abusive_frames,
    abusive_inputs,
    shared_queue_overloads,
    reliable_queue_disconnects,
    write_deadline_disconnects,
    room_panics,
    ticks,
    inputs_processed,
    snapshots,
    max_input_queue_depth,
    max_reliable_queue_depth,
    max_tick_delay_micros,
    max_tick_duration_micros,
    max_input_latency_micros,
    max_input_duration_micros
);

#[derive(Clone, Default, Serialize)]
pub(super) struct Rates {
    pub ticks_per_second: Option<f64>,
    pub inputs_per_second: Option<f64>,
    pub snapshots_per_second: Option<f64>,
    pub admission_rejections_per_second: Option<f64>,
    pub shared_queue_overloads_per_second: Option<f64>,
}
impl Rates {
    pub fn between(previous: Option<(&Metrics, Instant)>, current: &Metrics, now: Instant) -> Self {
        let Some((previous, at)) = previous else {
            return Self::default();
        };
        let seconds = now.saturating_duration_since(at).as_secs_f64();
        let rate = |old: u64, new: u64| {
            (seconds > 0.0)
                .then(|| new.checked_sub(old).map(|delta| delta as f64 / seconds))
                .flatten()
        };
        Self {
            ticks_per_second: rate(previous.ticks, current.ticks),
            inputs_per_second: rate(previous.inputs_processed, current.inputs_processed),
            snapshots_per_second: rate(previous.snapshots, current.snapshots),
            admission_rejections_per_second: rate(
                previous.admission_rejections,
                current.admission_rejections,
            ),
            shared_queue_overloads_per_second: rate(
                previous.shared_queue_overloads,
                current.shared_queue_overloads,
            ),
        }
    }
}

#[derive(Clone)]
pub(super) struct ResourceSample {
    pub at: Instant,
    pub unix_ms: u64,
    pub cpu_percent: Option<f64>,
    pub rss_bytes: Option<u64>,
    pub available: bool,
}
#[derive(Clone, Serialize)]
pub(super) struct ProcessData {
    pub pid: u32,
    pub state: &'static str,
    pub cpu_percent: Option<f64>,
    pub rss_bytes: Option<u64>,
    pub collected_at: Option<u64>,
    pub age_ms: Option<u64>,
}
impl ProcessData {
    pub fn at(sample: Option<&ResourceSample>, started_at: Instant, now: Instant) -> Self {
        let mut output = Self {
            pid: std::process::id(),
            state: if elapsed_ms(started_at, now) > STALE_MS {
                "unavailable"
            } else {
                "warming_up"
            },
            cpu_percent: None,
            rss_bytes: None,
            collected_at: None,
            age_ms: None,
        };
        if let Some(sample) = sample {
            let age = elapsed_ms(sample.at, now);
            output.collected_at = Some(sample.unix_ms);
            output.age_ms = Some(age);
            output.state = if age > STALE_MS {
                "stale"
            } else if !sample.available {
                "unavailable"
            } else if sample.cpu_percent.is_none() {
                "warming_up"
            } else {
                "ready"
            };
            if age <= STALE_MS && sample.available {
                output.cpu_percent = sample.cpu_percent;
                output.rss_bytes = sample.rss_bytes;
            }
        }
        output
    }
}
#[derive(Clone, Serialize)]
pub(super) struct RoomData {
    pub game_id: String,
    pub room_id: String,
    pub room_instance_id: String,
    pub public: bool,
    pub capacity: usize,
    pub connected_members: usize,
    pub reserved_members: usize,
    pub server_tick: u64,
    pub snapshot_seq: u64,
    pub summary_age_ms: u64,
    pub state: &'static str,
}
impl RoomData {
    pub fn at(room: &RoomObservation, cache_age_ms: u64) -> Self {
        let age = room.summary_age_ms.saturating_add(cache_age_ms);
        Self {
            game_id: room.game_id.clone(),
            room_id: room.room_id.clone(),
            room_instance_id: room.room_instance_id.clone(),
            public: room.public,
            capacity: room.capacity,
            connected_members: room.connected_members,
            reserved_members: room.reserved_members,
            server_tick: room.server_tick,
            snapshot_seq: room.snapshot_seq,
            summary_age_ms: age,
            state: if age > STALE_MS { "stale" } else { "ready" },
        }
    }
}

#[derive(Clone)]
pub(super) struct Sample {
    pub sample_seq: u64,
    pub collected_at: u64,
    pub at: Instant,
    pub interval_ms: Option<u64>,
    pub metrics: Metrics,
    pub rates: Rates,
    pub rooms: RoomTotals,
    pub resource: Option<ResourceSample>,
    pub resource_started_at: Instant,
}
#[derive(Serialize)]
pub(super) struct HistorySample<'a> {
    pub sample_seq: u64,
    pub collected_at: u64,
    pub interval_ms: Option<u64>,
    pub metrics: &'a Metrics,
    pub rates: &'a Rates,
    pub rooms: &'a RoomTotals,
    pub process: ProcessData,
}
impl Sample {
    pub fn response(&self) -> HistorySample<'_> {
        HistorySample {
            sample_seq: self.sample_seq,
            collected_at: self.collected_at,
            interval_ms: self.interval_ms,
            metrics: &self.metrics,
            rates: &self.rates,
            rooms: &self.rooms,
            process: ProcessData::at(self.resource.as_ref(), self.resource_started_at, self.at),
        }
    }
}
#[derive(Clone, Default)]
pub(super) struct Cache {
    pub latest: Option<Sample>,
    pub rooms: Vec<RoomObservation>,
    pub games: Vec<GameTotals>,
    pub uptime_ms: u64,
    pub stopping: bool,
    pub ended: bool,
    pub history: VecDeque<Sample>,
    pub resource_started_at: Option<Instant>,
}
impl Cache {
    pub fn collect(
        &mut self,
        observation: RuntimeObservation,
        resource: Option<ResourceSample>,
        now: Instant,
        unix_ms: u64,
    ) {
        let resource_started_at = *self.resource_started_at.get_or_insert(now);
        let metrics = Metrics::from(&observation.metrics);
        let rates = Rates::between(
            self.latest
                .as_ref()
                .map(|sample| (&sample.metrics, sample.at)),
            &metrics,
            now,
        );
        let mut totals = RoomTotals::default();
        let mut games: BTreeMap<String, RoomTotals> = observation
            .game_ids
            .iter()
            .map(|game| (game.clone(), RoomTotals::default()))
            .collect();
        for room in &observation.rooms {
            totals.add(room);
            games.entry(room.game_id.clone()).or_default().add(room);
        }
        let sample = Sample {
            sample_seq: self
                .latest
                .as_ref()
                .map_or(1, |sample| sample.sample_seq.saturating_add(1)),
            collected_at: unix_ms,
            at: now,
            interval_ms: self
                .latest
                .as_ref()
                .map(|sample| elapsed_ms(sample.at, now)),
            metrics,
            rates,
            rooms: totals,
            resource,
            resource_started_at,
        };
        self.history.push_back(sample.clone());
        while self.history.len() > HISTORY_LIMIT
            || self
                .history
                .front()
                .is_some_and(|sample| elapsed_ms(sample.at, now) > HISTORY_MS)
        {
            self.history.pop_front();
        }
        self.latest = Some(sample);
        self.rooms = observation.rooms;
        self.rooms
            .sort_by(|left, right| room_key(left).cmp(&room_key(right)));
        self.games = games
            .into_iter()
            .map(|(game_id, totals)| GameTotals { game_id, totals })
            .collect();
        self.uptime_ms = observation.uptime_ms;
        self.stopping = observation.stopping;
    }
    pub fn state(&self, now: Instant) -> &'static str {
        if self.ended || self.stopping {
            "stopped"
        } else if let Some(latest) = &self.latest {
            if elapsed_ms(latest.at, now) > STALE_MS {
                "stale"
            } else {
                "ready"
            }
        } else {
            "warming_up"
        }
    }
}
pub(super) fn elapsed_ms(at: Instant, now: Instant) -> u64 {
    now.saturating_duration_since(at)
        .as_millis()
        .min(u64::MAX as u128) as u64
}
pub(super) fn room_key(room: &RoomObservation) -> (&str, &str, &str) {
    (&room.game_id, &room.room_id, &room.room_instance_id)
}

#[derive(Deserialize, Serialize)]
pub(super) struct Cursor {
    pub boot_id: String,
    pub game_id: Option<String>,
    pub visibility: String,
    pub q: String,
    pub key: (String, String, String),
}
impl Cursor {
    pub fn encode(&self) -> String {
        serde_json::to_vec(self)
            .expect("primitive cursor serialization")
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
    pub fn decode(value: &str) -> Option<Self> {
        if value.len() > 8192 || !value.len().is_multiple_of(2) {
            return None;
        }
        let bytes: Option<Vec<_>> = value
            .as_bytes()
            .chunks_exact(2)
            .map(|chunk| {
                let value = std::str::from_utf8(chunk).ok()?;
                u8::from_str_radix(value, 16).ok()
            })
            .collect();
        serde_json::from_slice(&bytes?).ok()
    }
}
