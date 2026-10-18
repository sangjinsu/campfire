use tokio::time::Instant;

#[derive(Clone)]
pub(crate) struct Bucket {
    tokens: f64,
    updated: Instant,
}
impl Bucket {
    pub fn new(burst: u32) -> Self {
        Self {
            tokens: f64::from(burst),
            updated: Instant::now(),
        }
    }
    pub fn take(&mut self, rate: u32, burst: u32, now: Instant) -> bool {
        self.tokens = (self.tokens
            + now.saturating_duration_since(self.updated).as_secs_f64() * f64::from(rate))
        .min(f64::from(burst));
        self.updated = now;
        if self.tokens < 1.0 {
            false
        } else {
            self.tokens -= 1.0;
            true
        }
    }
}
