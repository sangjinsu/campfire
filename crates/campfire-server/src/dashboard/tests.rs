use super::*;
use campfire_runtime::{Campfire, Config, MetricsSnapshot, RoomObservation, RuntimeObservation};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    mpsc,
};

fn observation(ticks: u64) -> RuntimeObservation {
    RuntimeObservation {
        metrics: MetricsSnapshot {
            ticks,
            inputs_processed: ticks,
            ..MetricsSnapshot::default()
        },
        stopping: false,
        uptime_ms: 123,
        game_ids: vec!["empty".into(), "arena".into()],
        rooms: Vec::new(),
    }
}
fn room(id: &str, public: bool) -> RoomObservation {
    RoomObservation {
        game_id: "arena".into(),
        room_id: id.into(),
        room_instance_id: format!("instance-{id}"),
        public,
        capacity: 16,
        connected_members: 3,
        reserved_members: 2,
        server_tick: 19,
        snapshot_seq: 12,
        summary_age_ms: 100,
    }
}
fn shared(cache: Cache) -> Arc<Shared> {
    Arc::new(Shared {
        boot_id: "test-boot".into(),
        token_hash: [0; 32],
        cache: RwLock::new(Arc::new(cache)),
    })
}
async fn body(response: Response) -> Value {
    serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test(start_paused = true)]
async fn collector_warms_up_and_skips_missed_ticks_and_restarts_with_a_new_boot() {
    let runtime = Campfire::new(Config::default()).unwrap();
    let options = || {
        DashboardOptions::from_lookup(|_| Some("x".repeat(32)))
            .unwrap()
            .unwrap()
    };
    let mut dashboard = Dashboard::start(runtime.observer(), options()).unwrap();
    let Json(initial) = status(State(dashboard.shared.clone())).await;
    assert_eq!(initial["state"], "warming_up");
    assert_eq!(initial["data"], Value::Null);
    tokio::task::yield_now().await;
    assert_eq!(
        dashboard.shared.cache().latest.as_ref().unwrap().sample_seq,
        1
    );
    tokio::time::advance(Duration::from_secs(10)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        dashboard.shared.cache().latest.as_ref().unwrap().sample_seq,
        2
    );
    assert_eq!(
        dashboard
            .shared
            .cache()
            .latest
            .as_ref()
            .unwrap()
            .interval_ms,
        Some(10_000)
    );
    let mut restarted = Dashboard::start(runtime.observer(), options()).unwrap();
    assert_ne!(dashboard.shared.boot_id, restarted.shared.boot_id);
    assert!(restarted.shared.cache().history.is_empty());
    dashboard.shutdown().await;
    restarted.shutdown().await;
    runtime.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn history_bounds_count_and_monotonic_age_without_filling_skipped_intervals() {
    let start = Instant::now();
    let mut cache = Cache::default();
    for second in 0..1001 {
        cache.collect(
            observation(second),
            None,
            start + Duration::from_secs(second),
            1_000_000 + second * 1000,
        );
    }
    assert_eq!(cache.history.len(), 900);
    assert_eq!(cache.history.front().unwrap().sample_seq, 102);
    cache.collect(
        observation(1002),
        None,
        start + Duration::from_secs(2001),
        3_001_000,
    );
    assert_eq!(cache.history.len(), 1);
    assert_eq!(cache.latest.as_ref().unwrap().sample_seq, 1002);
    assert_eq!(cache.latest.as_ref().unwrap().interval_ms, Some(1_001_000));
    assert!(
        (cache
            .latest
            .as_ref()
            .unwrap()
            .rates
            .ticks_per_second
            .unwrap()
            - 2.0 / 1001.0)
            .abs()
            < 0.0000001
    );
}

#[tokio::test(start_paused = true)]
async fn rate_counter_decrease_is_unknown_and_maxima_remain_lifetime_values() {
    let start = Instant::now();
    let mut cache = Cache::default();
    let mut first = observation(100);
    first.metrics.max_tick_duration_micros = 99_000;
    cache.collect(first, None, start, 0);
    assert!(
        cache
            .latest
            .as_ref()
            .unwrap()
            .rates
            .ticks_per_second
            .is_none()
    );
    let mut next = observation(10);
    next.metrics.snapshots = 12;
    next.metrics.max_tick_duration_micros = 99_000;
    cache.collect(next, None, start + Duration::from_secs(4), 4000);
    let sample = cache.latest.as_ref().unwrap();
    assert!(sample.rates.ticks_per_second.is_none());
    assert_eq!(sample.rates.snapshots_per_second, Some(3.0));
    assert_eq!(sample.metrics.max_tick_duration_micros, 99_000);
}

#[tokio::test(start_paused = true)]
async fn frozen_cache_and_actor_resource_ages_become_stale_at_response_time() {
    let start = Instant::now();
    let resource = ResourceSample {
        at: start,
        unix_ms: 0,
        cpu_percent: Some(0.0),
        rss_bytes: Some(1024),
        available: true,
    };
    let mut cache = Cache::default();
    let mut current = observation(1);
    current.rooms.push(room("private", false));
    cache.collect(current, Some(resource), start, 0);
    assert_eq!(cache.state(start + Duration::from_secs(3)), "ready");
    tokio::time::advance(Duration::from_millis(3101)).await;
    let shared = shared(cache);
    let Json(status) = status(State(shared.clone())).await;
    assert_eq!(status["state"], "stale");
    assert_eq!(status["data"]["process"]["state"], "stale");
    assert_eq!(status["data"]["process"]["cpu_percent"], Value::Null);
    let rooms = body(rooms(State(shared.clone()), Ok(Query(RoomsQuery::default()))).await).await;
    assert_eq!(rooms["data"]["rooms"][0]["state"], "stale");
    assert_eq!(rooms["data"]["rooms"][0]["summary_age_ms"], 3201);
    let history = body(history(State(shared), Ok(Query(HistoryQuery::default()))).await).await;
    assert_eq!(history["data"]["samples"][0]["process"]["cpu_percent"], 0.0);
    assert_eq!(history["data"]["samples"][0]["process"]["age_ms"], 0);
}

#[tokio::test(start_paused = true)]
async fn room_keyset_cursor_is_bound_to_boot_and_filters_and_private_metadata_is_visible() {
    let mut cache = Cache::default();
    let mut current = observation(1);
    current.rooms = vec![room("c", false), room("a", true), room("b", false)];
    cache.collect(current, None, Instant::now(), 0);
    let shared = shared(cache);
    let page = body(
        rooms(
            State(shared.clone()),
            Ok(Query(RoomsQuery {
                limit: Some(1),
                ..RoomsQuery::default()
            })),
        )
        .await,
    )
    .await;
    assert_eq!(page["data"]["total_filtered"], 3);
    assert_eq!(page["data"]["rooms"][0]["room_id"], "a");
    let cursor = page["data"]["next_cursor"].as_str().unwrap().to_owned();
    let page = body(
        rooms(
            State(shared.clone()),
            Ok(Query(RoomsQuery {
                cursor: Some(cursor.clone()),
                limit: Some(1),
                ..RoomsQuery::default()
            })),
        )
        .await,
    )
    .await;
    assert_eq!(page["data"]["rooms"][0]["room_id"], "b");
    assert_eq!(page["data"]["rooms"][0]["public"], false);
    assert_eq!(page["data"]["rooms"][0]["reserved_members"], 2);
    assert_eq!(
        rooms(
            State(shared.clone()),
            Ok(Query(RoomsQuery {
                cursor: Some(cursor),
                q: Some("b".into()),
                ..RoomsQuery::default()
            }))
        )
        .await
        .status(),
        StatusCode::BAD_REQUEST
    );
    let private = body(
        rooms(
            State(shared),
            Ok(Query(RoomsQuery {
                visibility: Some("private".into()),
                q: Some("c".into()),
                ..RoomsQuery::default()
            })),
        )
        .await,
    )
    .await;
    assert_eq!(private["data"]["total_filtered"], 1);
    assert_eq!(private["data"]["rooms"][0]["room_id"], "c");
}

#[tokio::test(start_paused = true)]
async fn history_restart_and_evicted_cursor_reset_without_synthetic_points() {
    let start = Instant::now();
    let mut cache = Cache::default();
    for seq in 0..902 {
        cache.collect(observation(seq), None, start, seq);
    }
    let shared = shared(cache);
    for (boot, after, reset, count) in [
        ("test-boot", 1, true, 900),
        ("previous-boot", 902, true, 900),
        ("test-boot", 901, false, 1),
        ("test-boot", 902, false, 0),
        ("test-boot", 999, true, 900),
    ] {
        let result = body(
            history(
                State(shared.clone()),
                Ok(Query(HistoryQuery {
                    after_seq: Some(after),
                    boot_id: Some(boot.into()),
                    limit: None,
                })),
            )
            .await,
        )
        .await;
        assert_eq!(result["data"]["reset"], reset);
        assert_eq!(result["data"]["samples"].as_array().unwrap().len(), count);
    }
    tokio::time::advance(Duration::from_secs(901)).await;
    let result = body(history(State(shared), Ok(Query(HistoryQuery::default()))).await).await;
    assert!(result["data"]["samples"].as_array().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn resource_warmup_failure_and_multicore_values_are_explicit() {
    let mut sampler = process::SamplerState::default();
    let start = Instant::now();
    let first = sampler.record(Some((0.0, 1000)), start, 0);
    assert_eq!(
        ProcessData::at(Some(&first), start, start).state,
        "warming_up"
    );
    assert!(first.cpu_percent.is_none());
    let second = sampler.record(Some((0.0, 2000)), start + Duration::from_secs(1), 1000);
    assert_eq!(second.cpu_percent, Some(0.0));
    let third = sampler.record(Some((250.0, 3000)), start + Duration::from_secs(2), 2000);
    assert_eq!(third.cpu_percent, Some(250.0));
    let failure = sampler.record(None, start + Duration::from_secs(3), 3000);
    let failure = ProcessData::at(Some(&failure), start, start + Duration::from_secs(3));
    assert_eq!(failure.state, "unavailable");
    assert!(failure.rss_bytes.is_none());
}

#[tokio::test(start_paused = true)]
async fn failed_initial_refresh_cannot_initialize_the_cpu_baseline() {
    let start = Instant::now();
    let mut sampler = process::SamplerState::default();
    sampler.record(None, start, 0);
    let first_success = sampler.record(Some((0.0, 1000)), start + Duration::from_secs(1), 1000);
    assert!(first_success.cpu_percent.is_none());
}

struct BlockingProvider {
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
    calls: Arc<AtomicUsize>,
}
impl process::Provider for BlockingProvider {
    fn refresh(&mut self) -> Option<(f64, u64)> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.send(()).unwrap();
        self.release.recv().unwrap();
        Some((0.0, 1024))
    }
}

#[tokio::test]
async fn blocked_os_worker_does_not_stop_collection_and_shutdown_has_a_deadline() {
    let runtime = Campfire::new(Config::default()).unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let worker = process::ProcessWorker::with_provider(BlockingProvider {
        entered: entered_tx,
        release: release_rx,
        calls: calls.clone(),
    })
    .unwrap();
    entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let mut dashboard = Dashboard::with_worker(
        runtime.observer(),
        DashboardOptions::from_lookup(|_| Some("x".repeat(32)))
            .unwrap()
            .unwrap(),
        worker,
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(3100)).await;
    let cache = dashboard.shared.cache();
    assert!(cache.latest.as_ref().unwrap().sample_seq >= 3);
    assert_eq!(
        ProcessData::at(
            cache.latest.as_ref().unwrap().resource.as_ref(),
            cache.latest.as_ref().unwrap().resource_started_at,
            Instant::now()
        )
        .state,
        "unavailable"
    );
    let started = std::time::Instant::now();
    dashboard.shutdown().await;
    assert!(started.elapsed() < Duration::from_millis(2500));
    assert!(started.elapsed() >= Duration::from_millis(1900));
    dashboard.shutdown().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    release_tx.send(()).unwrap();
    runtime.shutdown().await;
}

#[test]
fn drop_does_not_join_a_blocked_os_call() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker = process::ProcessWorker::with_provider(BlockingProvider {
        entered: entered_tx,
        release: release_rx,
        calls: Arc::new(AtomicUsize::new(0)),
    })
    .unwrap();
    entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let started = std::time::Instant::now();
    drop(worker);
    assert!(started.elapsed() < Duration::from_millis(100));
    release_tx.send(()).unwrap();
}

struct EndsBeforePublishing {
    entered: mpsc::Sender<()>,
}
impl process::Provider for EndsBeforePublishing {
    fn refresh(&mut self) -> Option<(f64, u64)> {
        self.entered.send(()).unwrap();
        // Bypass the panic hook to model an unexpectedly terminated sampler quietly.
        std::panic::resume_unwind(Box::new("injected process worker termination"));
    }
}

#[tokio::test(start_paused = true)]
async fn terminated_worker_before_first_publish_becomes_unavailable() {
    let runtime = Campfire::new(Config::default()).unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let worker = process::ProcessWorker::with_provider(EndsBeforePublishing {
        entered: entered_tx,
    })
    .unwrap();
    entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let mut dashboard = Dashboard::with_worker(
        runtime.observer(),
        DashboardOptions::from_lookup(|_| Some("x".repeat(32)))
            .unwrap()
            .unwrap(),
        worker,
    )
    .unwrap();
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(3100)).await;
    tokio::task::yield_now().await;
    let Json(response) = status(State(dashboard.shared.clone())).await;
    assert_eq!(response["state"], "ready");
    assert_eq!(response["data"]["process"]["state"], "unavailable");
    assert_eq!(response["data"]["process"]["collected_at"], Value::Null);
    assert_eq!(response["data"]["process"]["rss_bytes"], Value::Null);
    dashboard.shutdown().await;
    runtime.shutdown().await;
}

struct SucceedsThenBlocks {
    calls: usize,
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}
impl process::Provider for SucceedsThenBlocks {
    fn refresh(&mut self) -> Option<(f64, u64)> {
        self.calls += 1;
        if self.calls > 1 {
            self.entered.send(()).unwrap();
            self.release.recv().unwrap();
        }
        Some((0.0, 1024))
    }
}

#[tokio::test(start_paused = true)]
async fn stalled_worker_after_a_successful_publish_becomes_stale() {
    let runtime = Campfire::new(Config::default()).unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker = process::ProcessWorker::with_provider(SucceedsThenBlocks {
        calls: 0,
        entered: entered_tx,
        release: release_rx,
    })
    .unwrap();
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let mut dashboard = Dashboard::with_worker(
        runtime.observer(),
        DashboardOptions::from_lookup(|_| Some("x".repeat(32)))
            .unwrap()
            .unwrap(),
        worker,
    )
    .unwrap();
    tokio::task::yield_now().await;
    let Json(initial) = status(State(dashboard.shared.clone())).await;
    assert_eq!(initial["data"]["process"]["rss_bytes"], 1024);
    tokio::time::advance(Duration::from_millis(3100)).await;
    tokio::task::yield_now().await;
    let Json(response) = status(State(dashboard.shared.clone())).await;
    assert_eq!(response["state"], "ready");
    assert_eq!(response["data"]["process"]["state"], "stale");
    assert_eq!(response["data"]["process"]["rss_bytes"], Value::Null);
    release_tx.send(()).unwrap();
    dashboard.shutdown().await;
    runtime.shutdown().await;
}
