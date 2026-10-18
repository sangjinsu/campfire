use crate::{MetricsSnapshot, room::RoomRef, session::Inner};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{RwLock, Weak},
};
use tokio::{sync::watch, time::Instant};

/// A read-only handle that does not own the runtime or contact room actors.
#[derive(Clone)]
pub struct RuntimeObserver {
    pub(crate) inner: Weak<Inner>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RuntimeObservation {
    pub metrics: MetricsSnapshot,
    pub stopping: bool,
    pub uptime_ms: u64,
    pub game_ids: Vec<String>,
    pub rooms: Vec<RoomObservation>,
}

/// Deliberately contains no player identity, credentials, settings, or game data.
#[derive(Clone, Debug, Serialize)]
pub struct RoomObservation {
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
}

#[derive(Clone)]
pub(crate) struct PublishedRoom {
    pub summary: RoomObservation,
    pub published_at: Instant,
}

impl PublishedRoom {
    pub fn new(reference: &RoomRef, public: bool, capacity: usize) -> Self {
        Self {
            summary: RoomObservation {
                game_id: reference.game_id.clone(),
                room_id: reference.id.clone(),
                room_instance_id: reference.instance.clone(),
                public,
                capacity,
                connected_members: 0,
                reserved_members: 0,
                server_tick: 0,
                snapshot_seq: 0,
                summary_age_ms: 0,
            },
            published_at: Instant::now(),
        }
    }
}

#[derive(Default)]
pub(crate) struct ObservationDirectory {
    state: RwLock<DirectoryState>,
}

#[derive(Default)]
struct DirectoryState {
    game_ids: BTreeSet<String>,
    rooms: BTreeMap<(String, String, String), watch::Receiver<PublishedRoom>>,
}

impl ObservationDirectory {
    pub fn register_game(&self, game_id: String) {
        self.state
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .game_ids
            .insert(game_id);
    }

    pub fn insert(&self, reference: &RoomRef, receiver: watch::Receiver<PublishedRoom>) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|error| error.into_inner());
        // Failed admissions may release their manager slot before the supervisor runs.
        // Pruning closed publishers keeps this directory within the manager's room bound.
        state.rooms.retain(|(game_id, room_id, _), previous| {
            (game_id != &reference.game_id || room_id != &reference.id)
                && previous.has_changed().is_ok()
        });
        state.rooms.insert(
            (
                reference.game_id.clone(),
                reference.id.clone(),
                reference.instance.clone(),
            ),
            receiver,
        );
    }

    pub fn remove(&self, reference: &RoomRef) {
        self.state
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .rooms
            .remove(&(
                reference.game_id.clone(),
                reference.id.clone(),
                reference.instance.clone(),
            ));
    }

    fn snapshot(&self) -> (Vec<String>, Vec<RoomObservation>) {
        let (game_ids, receivers): (Vec<_>, Vec<_>) = {
            let state = self.state.read().unwrap_or_else(|error| error.into_inner());
            (
                state.game_ids.iter().cloned().collect(),
                state.rooms.values().cloned().collect(),
            )
        };
        let now = Instant::now();
        let rooms = receivers
            .into_iter()
            .filter_map(|receiver| {
                if receiver.has_changed().is_err() {
                    return None;
                }
                // Never retain a watch borrow while reading another room or serializing.
                let published = receiver.borrow().clone();
                if receiver.has_changed().is_err() {
                    return None;
                }
                let mut summary = published.summary;
                summary.summary_age_ms = now
                    .saturating_duration_since(published.published_at)
                    .as_millis() as u64;
                Some(summary)
            })
            .collect();
        (game_ids, rooms)
    }
}

impl RuntimeObserver {
    /// Returns only cached actor summaries; it never locks the session manager or game registry.
    pub fn snapshot(&self) -> Option<RuntimeObservation> {
        let inner = self.inner.upgrade()?;
        let metrics = inner.metrics_snapshot();
        let stopping = *inner.shutdown.borrow();
        let uptime_ms = inner.clock_origin.elapsed().as_millis() as u64;
        let (game_ids, rooms) = inner.observations.snapshot();
        Some(RuntimeObservation {
            metrics,
            stopping,
            uptime_ms,
            game_ids,
            rooms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(instance: &str) -> RoomRef {
        RoomRef {
            game_id: "a".into(),
            id: "room".into(),
            instance: instance.into(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn directory_ages_cached_summaries_and_omits_closed_publishers_before_supervision() {
        let directory = ObservationDirectory::default();
        let reference = reference("first");
        let (publisher, receiver) = watch::channel(PublishedRoom::new(&reference, false, 3));
        directory.insert(&reference, receiver);
        tokio::time::advance(std::time::Duration::from_millis(1250)).await;
        let (_, rooms) = directory.snapshot();
        assert_eq!(rooms.len(), 1);
        assert_eq!(rooms[0].summary_age_ms, 1250);
        drop(publisher);
        assert!(directory.snapshot().1.is_empty());
    }

    #[test]
    fn directory_late_old_close_preserves_replacement_and_prunes_closed_admissions() {
        let directory = ObservationDirectory::default();
        let old = reference("old");
        let (old_publisher, receiver) = watch::channel(PublishedRoom::new(&old, true, 3));
        directory.insert(&old, receiver);
        let new = reference("new");
        let (_publisher, receiver) = watch::channel(PublishedRoom::new(&new, false, 2));
        directory.insert(&new, receiver);
        directory.remove(&old);
        assert_eq!(directory.snapshot().1[0].room_instance_id, "new");
        assert_eq!(directory.state.read().unwrap().rooms.len(), 1);
        drop(old_publisher);
        let mut stale = reference("closed");
        stale.id = "failed-admission".into();
        let (publisher, receiver) = watch::channel(PublishedRoom::new(&stale, false, 1));
        directory.insert(&stale, receiver);
        drop(publisher);
        for i in 0..32 {
            let mut next = reference("next");
            next.id = format!("temporary-{i}");
            let (publisher, receiver) = watch::channel(PublishedRoom::new(&next, false, 1));
            directory.insert(&next, receiver);
            assert_eq!(directory.state.read().unwrap().rooms.len(), 2);
            drop(publisher);
        }
    }
}
