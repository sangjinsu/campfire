use crate::ConfigError;

/// Per-WebSocket token-bucket limits for control requests and invalid frames.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionLimits {
    pub control_rate_per_second: u32,
    pub control_burst: u32,
    pub invalid_rate_per_second: u32,
    pub invalid_burst: u32,
}

impl Default for ConnectionLimits {
    fn default() -> Self {
        Self {
            control_rate_per_second: 10,
            control_burst: 20,
            invalid_rate_per_second: 2,
            invalid_burst: 4,
        }
    }
}

impl ConnectionLimits {
    pub fn validate(&self) -> Result<(), ConfigError> {
        let values = [
            self.control_rate_per_second,
            self.control_burst,
            self.invalid_rate_per_second,
            self.invalid_burst,
        ];
        if values.contains(&0) {
            return Err(ConfigError(
                "connection rates and bursts must be positive".into(),
            ));
        }
        if values.iter().any(|value| *value > 1_000_000) {
            return Err(ConfigError(
                "connection rates and bursts exceed finite safety bounds".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuntimeOptions {
    pub connection_limits: ConnectionLimits,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rate::Bucket;
    use std::time::Duration;
    use tokio::time::{Instant, advance};

    #[test]
    fn defaults_and_finite_validation() {
        assert_eq!(
            ConnectionLimits::default(),
            ConnectionLimits {
                control_rate_per_second: 10,
                control_burst: 20,
                invalid_rate_per_second: 2,
                invalid_burst: 4,
            }
        );
        for field in 0..4 {
            for value in [0, u32::MAX] {
                let mut limits = ConnectionLimits::default();
                let selected = match field {
                    0 => &mut limits.control_rate_per_second,
                    1 => &mut limits.control_burst,
                    2 => &mut limits.invalid_rate_per_second,
                    _ => &mut limits.invalid_burst,
                };
                *selected = value;
                assert!(limits.validate().is_err());
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn control_bucket_refills_on_manual_clock() {
        let limits = ConnectionLimits {
            control_rate_per_second: 1,
            control_burst: 2,
            ..ConnectionLimits::default()
        };
        let mut bucket = Bucket::new(limits.control_burst);
        let take = |bucket: &mut Bucket| {
            bucket.take(
                limits.control_rate_per_second,
                limits.control_burst,
                Instant::now(),
            )
        };
        assert!(take(&mut bucket));
        assert!(take(&mut bucket));
        assert!(!take(&mut bucket));
        advance(Duration::from_secs(1)).await;
        assert!(take(&mut bucket));
        assert!(!take(&mut bucket));
    }
}
