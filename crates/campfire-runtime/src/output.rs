use crate::metrics::{Metrics, inc, max};
use serde_json::Value;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{mpsc, watch};

pub(crate) struct Output {
    pub reliable: mpsc::Sender<Value>,
    pub latest: watch::Sender<Option<Value>>,
    pub stop: watch::Sender<bool>,
    pub alive: AtomicBool,
    pub metrics: Arc<Metrics>,
}
impl Output {
    pub fn send(&self, value: Value) -> bool {
        if !self.alive.load(Ordering::Acquire) {
            return false;
        }
        match self.reliable.try_send(value) {
            Ok(()) => {
                max(
                    &self.metrics.max_reliable_queue_depth,
                    (self.reliable.max_capacity() - self.reliable.capacity()) as u64,
                );
                true
            }
            Err(_) => {
                inc(&self.metrics.reliable_queue_disconnects);
                self.close();
                false
            }
        }
    }
    pub fn close(&self) {
        self.alive.store(false, Ordering::Release);
        self.stop.send_replace(true);
    }
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }
}
