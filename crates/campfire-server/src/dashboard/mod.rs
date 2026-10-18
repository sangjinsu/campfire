//! Optional read-only administration with an independent environment-only secret.
mod model;
mod process;
#[cfg(test)]
mod tests;

use axum::{
    Json, Router,
    body::Body,
    extract::{Query, State},
    http::{HeaderValue, Request, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use campfire_runtime::RuntimeObserver;
use model::*;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fmt,
    sync::{Arc, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;
use tokio::{
    sync::watch,
    task::JoinHandle,
    time::{Instant, MissedTickBehavior},
};

/// This type deliberately implements neither Debug nor Serialize.
pub struct DashboardOptions {
    token_hash: [u8; 32],
}
#[derive(Debug)]
pub struct DashboardError(&'static str);
impl fmt::Display for DashboardError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}
impl Error for DashboardError {}
impl DashboardOptions {
    pub fn from_env() -> Result<Option<Self>, DashboardError> {
        match std::env::var("CAMPFIRE_ADMIN_TOKEN") {
            Ok(value) => Self::from_lookup(|_| Some(value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => Err(DashboardError(
                "CAMPFIRE_ADMIN_TOKEN must be 32..256 ASCII graphic bytes",
            )),
        }
    }
    /// Supplies an environment reader without changing process-global environment.
    pub fn from_lookup(
        read: impl FnOnce(&str) -> Option<String>,
    ) -> Result<Option<Self>, DashboardError> {
        let Some(value) = read("CAMPFIRE_ADMIN_TOKEN").filter(|value| !value.is_empty()) else {
            return Ok(None);
        };
        if !(32..=256).contains(&value.len()) || !value.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(DashboardError(
                "CAMPFIRE_ADMIN_TOKEN must be 32..256 ASCII graphic bytes",
            ));
        }
        Ok(Some(Self {
            token_hash: Sha256::digest(value.as_bytes()).into(),
        }))
    }
}
struct Shared {
    boot_id: String,
    token_hash: [u8; 32],
    cache: RwLock<Arc<Cache>>,
}
impl Shared {
    fn cache(&self) -> Arc<Cache> {
        self.cache
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
    fn envelope(&self, cache: &Cache, now: Instant, data: Value) -> Value {
        json!({"schema_version":1,"boot_id":self.boot_id,"state":cache.state(now),"sample_seq":cache.latest.as_ref().map_or(0, |sample| sample.sample_seq),"collected_at":cache.latest.as_ref().map(|sample| sample.collected_at),"cache_age_ms":cache.latest.as_ref().map(|sample| elapsed_ms(sample.at,now)),"data":data})
    }
    fn error(&self, status: StatusCode, code: &'static str, message: &'static str) -> Response {
        let cache = self.cache();
        let mut envelope = self.envelope(&cache, Instant::now(), Value::Null);
        envelope["error"] = json!({"code":code,"message":message});
        (status, Json(envelope)).into_response()
    }
}
/// Retain this handle for the complete lifetime of the HTTP server.
pub struct Dashboard {
    shared: Arc<Shared>,
    cancel: watch::Sender<bool>,
    collector: Option<JoinHandle<()>>,
    worker: process::ProcessWorker,
}
impl Dashboard {
    pub fn start(
        observer: RuntimeObserver,
        options: DashboardOptions,
    ) -> Result<Self, DashboardError> {
        Self::with_worker(observer, options, process::ProcessWorker::start()?)
    }
    fn with_worker(
        observer: RuntimeObserver,
        options: DashboardOptions,
        worker: process::ProcessWorker,
    ) -> Result<Self, DashboardError> {
        let mut entropy = [0; 16];
        getrandom::fill(&mut entropy)
            .map_err(|_| DashboardError("dashboard OS entropy unavailable"))?;
        let boot_id = entropy.iter().map(|byte| format!("{byte:02x}")).collect();
        let shared = Arc::new(Shared {
            boot_id,
            token_hash: options.token_hash,
            cache: RwLock::new(Arc::new(Cache::default())),
        });
        let resource = worker.latest();
        let resource_started_at = worker.started_at();
        let (cancel, mut cancelled) = watch::channel(false);
        let output = shared.clone();
        let collector = tokio::spawn(async move {
            let mut cache = Cache {
                resource_started_at: Some(resource_started_at),
                ..Cache::default()
            };
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    biased;
                    _ = cancelled.changed() => break,
                    _ = interval.tick() => {
                        if *cancelled.borrow() { break; }
                        match observer.snapshot() {
                            Some(observation) => {
                                let process = resource.lock().unwrap_or_else(|error| error.into_inner()).clone();
                                cache.collect(observation, process, Instant::now(), unix_ms());
                                *output.cache.write().unwrap_or_else(|error| error.into_inner()) = Arc::new(cache.clone());
                            }
                            None => {
                                cache.ended = true;
                                *output.cache.write().unwrap_or_else(|error| error.into_inner()) = Arc::new(cache);
                                break;
                            }
                        }
                    }
                }
            }
        });
        Ok(Self {
            shared,
            cancel,
            collector: Some(collector),
            worker,
        })
    }
    pub fn router(&self) -> Router {
        let api = Router::new()
            .route("/status", get(status).fallback(method_not_allowed))
            .route("/rooms", get(rooms).fallback(method_not_allowed))
            .route("/history", get(history).fallback(method_not_allowed))
            .fallback(api_not_found)
            .layer(middleware::from_fn_with_state(
                self.shared.clone(),
                authorize,
            ));
        Router::new()
            .route("/admin", get(index))
            .route("/admin/", get(index))
            .route("/admin/assets/app.js", get(javascript))
            .route("/admin/assets/style.css", get(stylesheet))
            .route("/admin/app.js", get(javascript))
            .route("/admin/style.css", get(stylesheet))
            .nest("/admin/api", api)
            .with_state(self.shared.clone())
            .layer(middleware::from_fn(security_headers))
    }
    pub async fn shutdown(&mut self) {
        self.cancel.send_replace(true);
        self.worker.cancel();
        if let Some(task) = self.collector.take() {
            task.abort();
            let _ = task.await;
        }
        self.worker.shutdown().await;
    }
}
impl Drop for Dashboard {
    fn drop(&mut self) {
        self.cancel.send_replace(true);
        self.worker.cancel();
        if let Some(task) = &self.collector {
            task.abort();
        }
    }
}
async fn authorize(
    State(shared): State<Arc<Shared>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let mut values = request.headers().get_all(header::AUTHORIZATION).iter();
    let token = values
        .next()
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let valid = values.next().is_none()
        && token.is_some_and(|token| {
            let hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();
            bool::from(hash.ct_eq(&shared.token_hash))
        });
    if !valid {
        return shared.error(
            StatusCode::UNAUTHORIZED,
            "UNAUTHORIZED",
            "administrator authentication required",
        );
    }
    next.run(request).await
}
async fn security_headers(request: Request<Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'"));
    response
}
async fn index() -> Html<&'static str> {
    Html(include_str!("assets/index.html"))
}
async fn javascript() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("assets/app.js"),
    )
}
async fn stylesheet() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("assets/style.css"),
    )
}
async fn api_not_found(State(shared): State<Arc<Shared>>) -> Response {
    shared.error(
        StatusCode::NOT_FOUND,
        "NOT_FOUND",
        "administrator endpoint not found",
    )
}
async fn method_not_allowed(State(shared): State<Arc<Shared>>) -> Response {
    shared.error(
        StatusCode::METHOD_NOT_ALLOWED,
        "METHOD_NOT_ALLOWED",
        "administrator API is read-only",
    )
}
async fn status(State(shared): State<Arc<Shared>>) -> Json<Value> {
    let cache = shared.cache();
    let now = Instant::now();
    let data = cache.latest.as_ref().map(|sample| json!({"uptime_ms":cache.uptime_ms,"stopping":cache.stopping || cache.ended,"metrics":sample.metrics,"rates":sample.rates,"rooms":sample.rooms,"games":cache.games,"process":ProcessData::at(sample.resource.as_ref(),sample.resource_started_at,now)}));
    Json(shared.envelope(&cache, now, data.unwrap_or(Value::Null)))
}
#[derive(Default, Deserialize)]
struct RoomsQuery {
    game_id: Option<String>,
    visibility: Option<String>,
    q: Option<String>,
    limit: Option<usize>,
    cursor: Option<String>,
}
async fn rooms(
    State(shared): State<Arc<Shared>>,
    query: Result<Query<RoomsQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return shared.error(
            StatusCode::BAD_REQUEST,
            "INVALID_QUERY",
            "invalid room query",
        );
    };
    let limit = query.limit.unwrap_or(100);
    let visibility = query.visibility.unwrap_or_else(|| "all".into());
    let q = query.q.unwrap_or_default();
    if !(1..=100).contains(&limit)
        || !matches!(visibility.as_str(), "all" | "public" | "private")
        || q.len() > 256
        || query.game_id.as_ref().is_some_and(|game| game.len() > 256)
    {
        return shared.error(
            StatusCode::BAD_REQUEST,
            "INVALID_QUERY",
            "invalid room filter or page size",
        );
    }
    let cursor = match query.cursor {
        None => None,
        Some(value) => match Cursor::decode(&value) {
            Some(cursor)
                if cursor.boot_id == shared.boot_id
                    && cursor.game_id == query.game_id
                    && cursor.visibility == visibility
                    && cursor.q == q =>
            {
                Some(cursor)
            }
            _ => {
                return shared.error(
                    StatusCode::BAD_REQUEST,
                    "INVALID_CURSOR",
                    "room cursor is invalid for this query",
                );
            }
        },
    };
    let cache = shared.cache();
    let now = Instant::now();
    let age = cache
        .latest
        .as_ref()
        .map_or(0, |sample| elapsed_ms(sample.at, now));
    let filtered: Vec<_> = cache
        .rooms
        .iter()
        .filter(|room| {
            query
                .game_id
                .as_ref()
                .is_none_or(|game| &room.game_id == game)
                && (visibility == "all" || room.public == (visibility == "public"))
                && room.room_id.contains(&q)
        })
        .collect();
    let mut page = filtered.iter().copied().filter(|room| {
        cursor.as_ref().is_none_or(|cursor| {
            room_key(room)
                > (
                    cursor.key.0.as_str(),
                    cursor.key.1.as_str(),
                    cursor.key.2.as_str(),
                )
        })
    });
    let rows: Vec<_> = page.by_ref().take(limit).collect();
    let next_cursor = if page.next().is_some() {
        rows.last().map(|room| {
            Cursor {
                boot_id: shared.boot_id.clone(),
                game_id: query.game_id,
                visibility,
                q,
                key: (
                    room.game_id.clone(),
                    room.room_id.clone(),
                    room.room_instance_id.clone(),
                ),
            }
            .encode()
        })
    } else {
        None
    };
    let rows: Vec<_> = rows
        .into_iter()
        .map(|room| RoomData::at(room, age))
        .collect();
    Json(shared.envelope(
        &cache,
        now,
        json!({"rooms":rows,"next_cursor":next_cursor,"total_filtered":filtered.len()}),
    ))
    .into_response()
}
#[derive(Default, Deserialize)]
struct HistoryQuery {
    after_seq: Option<u64>,
    boot_id: Option<String>,
    limit: Option<usize>,
}
async fn history(
    State(shared): State<Arc<Shared>>,
    query: Result<Query<HistoryQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return shared.error(
            StatusCode::BAD_REQUEST,
            "INVALID_QUERY",
            "invalid history query",
        );
    };
    let limit = query.limit.unwrap_or(HISTORY_LIMIT);
    if !(1..=HISTORY_LIMIT).contains(&limit) {
        return shared.error(
            StatusCode::BAD_REQUEST,
            "INVALID_QUERY",
            "invalid history page size",
        );
    }
    let cache = shared.cache();
    let now = Instant::now();
    let retained: Vec<_> = cache
        .history
        .iter()
        .filter(|sample| elapsed_ms(sample.at, now) <= HISTORY_MS)
        .collect();
    let oldest_seq = retained.first().map(|sample| sample.sample_seq);
    let latest_seq = cache.latest.as_ref().map_or(0, |sample| sample.sample_seq);
    let reset = query.after_seq.is_none()
        || query.boot_id.as_deref() != Some(shared.boot_id.as_str())
        || query.after_seq.is_some_and(|after| {
            after > latest_seq || oldest_seq.is_some_and(|oldest| after < oldest.saturating_sub(1))
        });
    let samples: Vec<_> = retained
        .into_iter()
        .filter(|sample| {
            reset
                || query
                    .after_seq
                    .is_none_or(|after| sample.sample_seq > after)
        })
        .take(limit)
        .map(|sample| sample.response())
        .collect();
    Json(shared.envelope(
        &cache,
        now,
        json!({"samples":samples,"reset":reset,"oldest_seq":oldest_seq}),
    ))
    .into_response()
}
fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
