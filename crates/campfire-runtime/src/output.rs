use crate::metrics::{Metrics, inc, max};
use axum::extract::ws::Utf8Bytes;
use serde_json::Value;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{mpsc, watch};

/// A regular snapshot already encoded as its WebSocket text frame.
///
/// The room encodes the shared state once per publication; cloning this for a
/// writer only bumps reference counts.
#[derive(Clone)]
pub(crate) struct SnapshotFrame {
    pub instance: Arc<str>,
    pub seq: u64,
    pub text: Utf8Bytes,
}

pub(crate) struct Output {
    pub reliable: mpsc::Sender<Value>,
    pub latest: watch::Sender<Option<SnapshotFrame>>,
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
