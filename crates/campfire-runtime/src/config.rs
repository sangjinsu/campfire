use std::{fmt, time::Duration};

#[derive(Clone, Debug)]
pub struct Config {
    pub room_capacity: usize,
    pub tick_hz: u32,
    pub snapshot_hz: u32,
    pub guest_ttl: Duration,
    pub reconnect_grace: Duration,
    pub auth_timeout: Duration,
    pub cleanup_interval: Duration,
    pub max_frame_bytes: usize,
    pub input_rate_per_second: u32,
    pub input_burst: u32,
    pub room_input_capacity: usize,
    pub reliable_capacity: usize,
    pub request_cache_capacity: usize,
    pub max_input_seq_jump: u64,
    pub max_sessions: usize,
    pub max_connections: usize,
    pub max_unauthenticated_connections: usize,
    pub max_rooms: usize,
    pub max_rate_keys: usize,
    pub guest_rate_per_second: u32,
    pub guest_burst: u32,
    pub global_guest_rate_per_second: u32,
    pub global_guest_burst: u32,
    pub rate_key_ttl: Duration,
    pub write_timeout: Duration,
    pub max_write_timeouts: u32,
    pub max_abuse_violations: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            room_capacity: 16,
            tick_hz: 30,
            snapshot_hz: 15,
            guest_ttl: Duration::from_secs(86_400),
            reconnect_grace: Duration::from_secs(30),
            auth_timeout: Duration::from_secs(5),
            cleanup_interval: Duration::from_millis(100),
            max_frame_bytes: 16 * 1024,
            input_rate_per_second: 60,
            input_burst: 30,
            room_input_capacity: 1024,
            reliable_capacity: 64,
            request_cache_capacity: 128,
            max_input_seq_jump: 10_000,
            max_sessions: 10_000,
            max_connections: 4096,
            max_unauthenticated_connections: 256,
            max_rooms: 1024,
            max_rate_keys: 4096,
            guest_rate_per_second: 2,
            guest_burst: 10,
            global_guest_rate_per_second: 100,
            global_guest_burst: 200,
            rate_key_ttl: Duration::from_secs(60),
            write_timeout: Duration::from_secs(2),
            max_write_timeouts: 2,
            max_abuse_violations: 5,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError(pub String);
impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ConfigError {}

impl Config {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.tick_hz == 0
            || self.tick_hz > 1000
            || self.snapshot_hz == 0
            || self.snapshot_hz > self.tick_hz
        {
            return Err(ConfigError(
                "tick_hz must be 1..=1000 and snapshot_hz must be 1..=tick_hz".into(),
            ));
        }
        let capacities = [
            self.room_capacity,
            self.max_frame_bytes,
            self.room_input_capacity,
            self.reliable_capacity,
            self.request_cache_capacity,
            self.max_sessions,
            self.max_connections,
            self.max_unauthenticated_connections,
            self.max_rooms,
            self.max_rate_keys,
        ];
        let rates = [
            self.input_rate_per_second,
            self.input_burst,
            self.guest_rate_per_second,
            self.guest_burst,
            self.global_guest_rate_per_second,
            self.global_guest_burst,
            self.max_write_timeouts,
            self.max_abuse_violations,
        ];
        let durations = [
            self.guest_ttl,
            self.reconnect_grace,
            self.auth_timeout,
            self.cleanup_interval,
            self.rate_key_ttl,
            self.write_timeout,
        ];
        if capacities.contains(&0)
            || rates.contains(&0)
            || self.max_input_seq_jump == 0
            || durations.iter().any(Duration::is_zero)
        {
            return Err(ConfigError(
                "capacities, rates, sequence jump and durations must be positive and finite".into(),
            ));
        }
        if capacities.iter().any(|value| *value > 1_048_576)
            || rates.iter().any(|value| *value > 1_000_000)
            || self.max_frame_bytes < 256
            || self.room_capacity > 4096
            || durations
                .iter()
                .any(|value| *value > Duration::from_secs(366 * 24 * 3600))
        {
            return Err(ConfigError(
                "configuration exceeds finite resource or time safety bounds".into(),
            ));
        }
        Ok(())
    }
    pub fn tick_interval(&self) -> Duration {
        Duration::from_secs_f64(1.0 / f64::from(self.tick_hz))
    }
}
