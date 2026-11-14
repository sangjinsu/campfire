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

/// A reliable message encoded once as its WebSocket text frame.
///
/// `sync` is set for full syncs (`room.entered`, `room.resumed`) so the writer
/// knows which room instance and snapshot sequence later snapshots must follow.
#[derive(Clone, Debug)]
pub(crate) struct ReliableFrame {
    pub text: Utf8Bytes,
    pub sync: Option<(Arc<str>, u64)>,
}
impl From<&Value> for ReliableFrame {
    fn from(value: &Value) -> Self {
        let kind = value["type"].as_str().unwrap_or_default();
        // Regular snapshots only travel through the latest slot, where they are
        // deduplicated; one sent here would bypass that check.
        debug_assert_ne!(kind, "snapshot");
        let sync = matches!(kind, "room.entered" | "room.resumed").then(|| {
            (
                value["room_instance_id"]
                    .as_str()
                    .unwrap_or_default()
                    .into(),
                value["snapshot_seq"].as_u64().unwrap_or(0),
            )
        });
        // An exactly sized buffer converts without a second allocation, and frames kept
        // in a response cache do not hold on to spare capacity.
        let mut text = value.to_string();
        text.shrink_to_fit();
        Self {
            text: text.into(),
            sync,
        }
    }
}
#[cfg(test)]
impl PartialEq<Value> for ReliableFrame {
    fn eq(&self, other: &Value) -> bool {
        serde_json::from_str::<Value>(&self.text).is_ok_and(|value| &value == other)
    }
}

pub(crate) struct Output {
    pub reliable: mpsc::Sender<ReliableFrame>,
    pub latest: watch::Sender<Option<SnapshotFrame>>,
    pub stop: watch::Sender<bool>,
    pub alive: AtomicBool,
    pub metrics: Arc<Metrics>,
}
impl Output {
    pub fn send(&self, value: Value) -> bool {
        // Checked here as well so a closed connection is not encoded for.
        if !self.alive.load(Ordering::Acquire) {
            return false;
        }
        self.send_frame(ReliableFrame::from(&value))
    }
    /// Queues an already encoded message, for callers that also keep the frame.
    pub fn send_frame(&self, frame: ReliableFrame) -> bool {
        if !self.alive.load(Ordering::Acquire) {
            return false;
        }
        match self.reliable.try_send(frame) {
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
