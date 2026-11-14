use crate::{
    Campfire,
    connection_limits::ConnectionLimits,
    metrics::inc,
    output::{Output, ReliableFrame, SnapshotFrame},
    protocol::{self, Command, Error, ResponseCache},
    rate::Bucket,
    session::TokenHash,
    source::{SourceSlot, client_address, forwarded_entries, source_key},
};
use axum::{
    Json,
    extract::{
        ConnectInfo, Path, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::{
    sync::{OwnedSemaphorePermit, mpsc, watch},
    time::Instant,
};

pub(crate) async fn health() -> Json<Value> {
    Json(json!({"status":"ok"}))
}
/// Readiness for load balancers: unlike `/health`, it fails once shutdown has begun.
pub(crate) async fn ready(State(runtime): State<Campfire>) -> Response {
    if *runtime.inner.shutdown.borrow() {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"status":"stopping"})),
        )
            .into_response()
    } else {
        Json(json!({"status":"ready"})).into_response()
    }
}
type PeerAddress = Result<ConnectInfo<SocketAddr>, axum::extract::rejection::ExtensionRejection>;

/// The shared key of requests served without `ConnectInfo`.
const UNKNOWN_SOURCE: &str = "unknown";

/// The admission key for a request. `X-Forwarded-For` counts only when the peer is a configured
/// trusted proxy; a router served without `ConnectInfo` shares [`UNKNOWN_SOURCE`].
fn request_source(runtime: &Campfire, peer: PeerAddress, headers: &HeaderMap) -> String {
    let Ok(ConnectInfo(peer)) = peer else {
        return UNKNOWN_SOURCE.into();
    };
    let mut forwarded = Vec::new();
    for value in headers.get_all("x-forwarded-for") {
        match value.to_str() {
            Ok(value) => forwarded.extend(forwarded_entries(value)),
            // An unreadable line hides which entry the proxy appended, so none of them is used.
            Err(_) => {
                forwarded.clear();
                break;
            }
        }
    }
    source_key(client_address(
        peer.ip(),
        forwarded.into_iter(),
        &runtime.inner.config.trusted_proxies,
    ))
}
pub(crate) async fn guest(
    State(runtime): State<Campfire>,
    Path(game): Path<String>,
    source: Result<ConnectInfo<SocketAddr>, axum::extract::rejection::ExtensionRejection>,
    headers: HeaderMap,
) -> Response {
    let source = request_source(&runtime, source, &headers);
    match runtime.inner.issue(&game, source).await {
        Ok(value) => (StatusCode::CREATED, Json(value)).into_response(),
        Err(error) => http_error(error),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AccountRequest {
    credential: String,
}
pub(crate) async fn account(
    State(runtime): State<Campfire>,
    Path(game): Path<String>,
    source: Result<ConnectInfo<SocketAddr>, axum::extract::rejection::ExtensionRejection>,
    headers: HeaderMap,
    body: Result<Json<AccountRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(body) = match body {
        Ok(body) => body,
        Err(rejection) if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(
                    Error::new("FRAME_TOO_LARGE", "request body exceeds configured limit")
                        .value(None),
                ),
            )
                .into_response();
        }
        Err(rejection) if rejection.status() == StatusCode::UNSUPPORTED_MEDIA_TYPE => {
            return (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                Json(
                    Error::new(
                        "INVALID_MESSAGE",
                        "application/json content type is required",
                    )
                    .value(None),
                ),
            )
                .into_response();
        }
        Err(_) => {
            return http_error(Error::new(
                "INVALID_MESSAGE",
                "invalid account authentication request",
            ));
        }
    };
    let source = request_source(&runtime, source, &headers);
    match runtime
        .inner
        .issue_account(&game, &body.credential, source)
        .await
    {
        Ok(value) => (StatusCode::CREATED, Json(value)).into_response(),
        Err(error) => http_error(error),
    }
}
fn http_error(error: Error) -> Response {
    let status = match error.code.as_str() {
        "GAME_NOT_FOUND" => StatusCode::NOT_FOUND,
        "RATE_LIMITED" => StatusCode::TOO_MANY_REQUESTS,
        "ACCOUNT_AUTH_DISABLED" => StatusCode::NOT_IMPLEMENTED,
        "INVALID_MESSAGE" => StatusCode::BAD_REQUEST,
        "UNAUTHENTICATED" => StatusCode::UNAUTHORIZED,
        "AUTH_TIMEOUT" => StatusCode::GATEWAY_TIMEOUT,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    };
    (status, Json(error.value(None))).into_response()
}
pub(crate) async fn upgrade(
    State(runtime): State<Campfire>,
    Path(game): Path<String>,
    source: PeerAddress,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !runtime.inner.has_game(&game) {
        return http_error(Error::new("GAME_NOT_FOUND", "game is not registered"));
    }
    if *runtime.inner.shutdown.borrow() {
        return http_error(Error::busy());
    }
    let connection = match runtime.inner.connections.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            inc(&runtime.inner.metrics.admission_rejections);
            return http_error(Error::busy());
        }
    };
    let unauthenticated = match runtime.inner.unauthenticated.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            inc(&runtime.inner.metrics.admission_rejections);
            return http_error(Error::busy());
        }
    };
    let source = request_source(&runtime, source, &headers);
    // Without peer addresses every connection is one source, so only the global cap applies.
    let source_slot = if source == UNKNOWN_SOURCE {
        None
    } else {
        let Some(slot) = runtime
            .inner
            .unauthenticated_sources
            .acquire(&source, runtime.inner.config.max_unauthenticated_per_source)
        else {
            inc(&runtime.inner.metrics.admission_rejections);
            return http_error(Error::busy());
        };
        Some(slot)
    };
    let unauthenticated = (unauthenticated, source_slot);
    // A bounded transport safety ceiling allows modest application-limit violations to be counted.
    let ceiling = runtime.inner.config.max_frame_bytes.saturating_mul(4);
    ws.max_frame_size(ceiling)
        .max_message_size(ceiling)
        .read_buffer_size(READ_BUFFER_BYTES)
        .write_buffer_size(0)
        .max_write_buffer_size(16 * 1024 * 1024)
        .on_upgrade(move |socket| {
            connection_task(runtime, game, socket, connection, unauthenticated)
        })
}
async fn connection_task(
    runtime: Campfire,
    game: String,
    socket: WebSocket,
    _connection: OwnedSemaphorePermit,
    unauthenticated: (OwnedSemaphorePermit, Option<SourceSlot>),
) {
    if *runtime.inner.shutdown.borrow() {
        return;
    }
    let config = runtime.inner.config.clone();
    let connection_limits = runtime.inner.connection_limits.clone();
    let (reliable, mut reliable_rx) = mpsc::channel(config.reliable_capacity);
    let (latest, mut latest_rx) = watch::channel::<Option<SnapshotFrame>>(None);
    let (stop, _) = watch::channel(false);
    let output = Arc::new(Output {
        reliable,
        latest,
        stop,
        alive: AtomicBool::new(true),
        metrics: runtime.inner.metrics.clone(),
    });
    let (mut sink, mut stream) = socket.split();
    let writer_output = output.clone();
    let writer_config = config.clone();
    let mut writer_stop = output.stop.subscribe();
    let mut global_stop = runtime.inner.shutdown.subscribe();
    // Milliseconds since `opened` at which the reader last received any frame, pongs included.
    let opened = Instant::now();
    let last_received = Arc::new(AtomicU64::new(0));
    let writer_received = last_received.clone();
    let writer = tokio::spawn(async move {
        let mut last_snapshot: Option<(Arc<str>, u64)> = None;
        let mut ping = tokio::time::interval_at(
            opened + writer_config.ping_interval,
            writer_config.ping_interval,
        );
        ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            if *global_stop.borrow() {
                break;
            }
            let next = tokio::select! {biased;
                value=reliable_rx.recv()=>value.map(Outgoing::Reliable),
                _=writer_stop.changed()=>None,
                _=global_stop.changed()=>None,
                // Polled before snapshots so a steady snapshot stream cannot starve liveness checks.
                _=ping.tick()=> {
                    let silent = opened.elapsed().saturating_sub(std::time::Duration::from_millis(
                        writer_received.load(Ordering::Relaxed),
                    ));
                    if silent >= writer_config.idle_timeout {
                        inc(&writer_output.metrics.idle_disconnects);
                        break;
                    }
                    match write_message(&mut sink, Message::Ping(Default::default()), &writer_config).await {
                        Ok(()) => continue,
                        Err(SendFailure::Deadline) => {
                            inc(&writer_output.metrics.write_deadline_disconnects);
                            break;
                        }
                        Err(SendFailure::Closed) => break,
                    }
                }
                changed=latest_rx.changed()=> {
                    if changed.is_err(){break;}
                    let frame=latest_rx.borrow_and_update().clone();
                    let Some(frame)=frame else{continue;};
                    Some(Outgoing::Snapshot(frame))
                }
            };
            let Some(next) = next else {
                break;
            };
            let (text, next_snapshot) = match next {
                Outgoing::Snapshot(frame) => {
                    if last_snapshot.as_ref().is_none_or(|(current, last)| {
                        **current != *frame.instance || frame.seq <= *last
                    }) {
                        continue;
                    }
                    (frame.text, Some((frame.instance, frame.seq)))
                }
                Outgoing::Reliable(frame) => (frame.text, frame.sync),
            };
            match write_message(&mut sink, Message::Text(text), &writer_config).await {
                Ok(()) => {
                    if next_snapshot.is_some() {
                        last_snapshot = next_snapshot;
                    }
                }
                Err(SendFailure::Deadline) => {
                    inc(&writer_output.metrics.write_deadline_disconnects);
                    break;
                }
                Err(SendFailure::Closed) => break,
            }
        }
        writer_output.close();
        let _ = tokio::time::timeout(writer_config.write_timeout, sink.close()).await;
    });
    let (requests, mut request_rx) = mpsc::channel(32);
    let reader_output = output.clone();
    let reader_config = config.clone();
    let reader_limits = connection_limits;
    let mut reader_stop = output.stop.subscribe();
    let reader = tokio::spawn(async move {
        let mut first = true;
        let mut violations = 0;
        let mut input_rate = Bucket::new(reader_config.input_burst);
        let mut control_rate = Bucket::new(reader_limits.control_burst);
        let mut invalid_rate = Bucket::new(reader_limits.invalid_burst);
        loop {
            let message = tokio::select! {biased;_=reader_stop.changed()=>break,message=stream.next()=>message};
            let Some(Ok(message)) = message else {
                break;
            };
            last_received.store(opened.elapsed().as_millis() as u64, Ordering::Relaxed);
            let text = match message {
                Message::Text(text) => text,
                Message::Close(_) => break,
                Message::Ping(_) | Message::Pong(_) => continue,
                Message::Binary(_) => {
                    if reject_invalid_frame(
                        &reader_output,
                        &mut invalid_rate,
                        &reader_limits,
                        &mut violations,
                        reader_config.max_abuse_violations,
                        first,
                        "INVALID_MESSAGE",
                        "binary messages are not supported",
                        None,
                    ) {
                        break;
                    }
                    continue;
                }
            };
            if text.len() > reader_config.max_frame_bytes {
                if reject_invalid_frame(
                    &reader_output,
                    &mut invalid_rate,
                    &reader_limits,
                    &mut violations,
                    reader_config.max_abuse_violations,
                    first,
                    "FRAME_TOO_LARGE",
                    "JSON frame exceeds configured limit",
                    None,
                ) {
                    break;
                }
                continue;
            }
            let request = match protocol::parse(text.as_str()) {
                Ok(request) => request,
                Err(code) => {
                    let id = serde_json::from_str::<Value>(text.as_str())
                        .ok()
                        .and_then(|value| {
                            value
                                .get("request_id")
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                        });
                    if reject_invalid_frame(
                        &reader_output,
                        &mut invalid_rate,
                        &reader_limits,
                        &mut violations,
                        reader_config.max_abuse_violations,
                        first,
                        code,
                        "invalid protocol envelope or command schema",
                        id.as_deref(),
                    ) {
                        break;
                    }
                    continue;
                }
            };
            if first {
                first = false;
                if !matches!(request.command, Command::Authenticate { .. }) {
                    reader_output.send(
                        Error::new("UNAUTHENTICATED", "first message must authenticate")
                            .value(Some(&request.id)),
                    );
                    break;
                }
            }
            let (within_rate, rate_message) = if matches!(
                request.command,
                Command::Input { .. } | Command::Execute { .. }
            ) {
                (
                    input_rate.take(
                        reader_config.input_rate_per_second,
                        reader_config.input_burst,
                        Instant::now(),
                    ),
                    "connection input rate exceeded",
                )
            } else {
                (
                    control_rate.take(
                        reader_limits.control_rate_per_second,
                        reader_limits.control_burst,
                        Instant::now(),
                    ),
                    "connection control rate exceeded",
                )
            };
            if !within_rate {
                inc(&reader_output.metrics.abusive_inputs);
                violations += 1;
                reader_output
                    .send(Error::new("RATE_LIMITED", rate_message).value(Some(&request.id)));
                if violations >= reader_config.max_abuse_violations {
                    break;
                }
                continue;
            }
            match requests.try_send(request) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(request)) => {
                    inc(&reader_output.metrics.shared_queue_overloads);
                    reader_output.send(Error::busy().value(Some(&request.id)));
                }
                Err(mpsc::error::TrySendError::Closed(_)) => break,
            }
        }
        reader_output.close();
    });
    let mut identity: Option<(TokenHash, u64)> = None;
    let mut deadline = Instant::now() + config.auth_timeout;
    let mut unauthenticated = Some(unauthenticated);
    let mut cache = ResponseCache::new(config.request_cache_capacity);
    let mut dispatcher_stop = output.stop.subscribe();
    loop {
        let request = tokio::select! {biased;
            _=dispatcher_stop.changed()=>break,
            _=tokio::time::sleep_until(deadline)=>{output.send(Error::new("UNAUTHENTICATED","authentication deadline or token lifetime expired").value(None));break;}
            request=request_rx.recv()=>{let Some(request)=request else{break;};request}
        };
        match cache.get(&request) {
            Ok(Some(response)) => {
                if !matches!(request.command, Command::Execute { .. }) {
                    output.send_frame(response);
                    continue;
                }
            }
            Err(error) => {
                output.send(error.value(Some(&request.id)));
                continue;
            }
            Ok(None) => {}
        }
        if request.game_id.as_ref().is_some_and(|id| id != &game) {
            let response = Error::new(
                "GAME_MISMATCH",
                "command game_id differs from connection game",
            )
            .value(Some(&request.id));
            let response = ReliableFrame::from(&response);
            output.send_frame(response.clone());
            cache.insert(&request, response);
            continue;
        }
        if identity.is_none() {
            let Command::Authenticate { token } = &request.command else {
                break;
            };
            match runtime
                .inner
                .authenticate(&game, token, &request.id, output.clone())
                .await
            {
                Ok((key, generation, expires, response)) => {
                    identity = Some((key, generation));
                    deadline = expires;
                    unauthenticated.take();
                    let response = ReliableFrame::from(&response);
                    output.send_frame(response.clone());
                    cache.insert(&request, response);
                }
                Err(error) => {
                    output.send(error.value(Some(&request.id)));
                    break;
                }
            }
            continue;
        }
        let (key, generation) = identity.expect("authenticated identity");
        match runtime
            .inner
            .command(key, generation, &game, output.clone(), &request)
            .await
        {
            Ok((response, already_enqueued)) => {
                // Encode once; the queue and the cache share the same frame.
                let response = ReliableFrame::from(&response);
                if !already_enqueued {
                    output.send_frame(response.clone());
                }
                cache.insert(&request, response);
                if matches!(request.command, Command::Logout) {
                    break;
                }
            }
            Err(error) => {
                let response = ReliableFrame::from(&error.value(Some(&request.id)));
                output.send_frame(response.clone());
                cache.insert(&request, response);
            }
        }
    }
    output.close();
    if let Some((key, generation)) = identity {
        runtime.inner.disconnect(key, generation).await;
    }
    reader.abort();
    let _ = reader.await;
    let _ = writer.await;
}

#[allow(clippy::too_many_arguments)]
fn reject_invalid_frame(
    output: &Output,
    rate: &mut Bucket,
    limits: &ConnectionLimits,
    violations: &mut u32,
    maximum_violations: u32,
    first: bool,
    code: &str,
    message: &str,
    request_id: Option<&str>,
) -> bool {
    inc(&output.metrics.abusive_frames);
    *violations = violations.saturating_add(1);
    let within_rate = rate.take(
        limits.invalid_rate_per_second,
        limits.invalid_burst,
        Instant::now(),
    );
    let error = if within_rate {
        Error::new(code, message)
    } else {
        Error::new("RATE_LIMITED", "connection invalid frame rate exceeded")
    };
    output.send(error.value(request_id));
    first || !within_rate || *violations >= maximum_violations
}

/// Bytes requested from the socket per read. Client messages are a few hundred bytes
/// and capped by `max_frame_bytes`, so the library default of 128 KiB per connection
/// only costs memory; larger messages still arrive, over several reads.
const READ_BUFFER_BYTES: usize = 4096;

/// One item for the connection writer: a queued reliable message or the latest snapshot frame.
enum Outgoing {
    Reliable(ReliableFrame),
    Snapshot(SnapshotFrame),
}

#[derive(Debug, PartialEq, Eq)]
enum SendFailure {
    Closed,
    Deadline,
}
async fn write_message<S>(
    sink: &mut S,
    message: Message,
    config: &crate::Config,
) -> Result<(), SendFailure>
where
    S: futures_util::Sink<Message> + Unpin,
{
    // Keep the same future alive: dropping SinkExt::send can lose a reliable message before poll_ready.
    let send = sink.send(message);
    tokio::pin!(send);
    for _ in 0..config.max_write_timeouts {
        match tokio::time::timeout(config.write_timeout, send.as_mut()).await {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(_)) => return Err(SendFailure::Closed),
            Err(_) => {}
        }
    }
    Err(SendFailure::Deadline)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        pin::Pin,
        sync::{
            Mutex,
            atomic::{AtomicBool, Ordering},
        },
        task::{Context, Poll, Waker},
        time::Duration,
    };
    struct GateSink {
        ready: Arc<AtomicBool>,
        wake: Arc<Mutex<Option<Waker>>>,
        sent: Arc<Mutex<Vec<Message>>>,
    }
    impl futures_util::Sink<Message> for GateSink {
        type Error = ();
        fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), ()>> {
            if self.ready.load(Ordering::Acquire) {
                Poll::Ready(Ok(()))
            } else {
                *self.wake.lock().unwrap() = Some(cx.waker().clone());
                Poll::Pending
            }
        }
        fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), ()> {
            self.sent.lock().unwrap().push(item);
            Ok(())
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), ()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), ()>> {
            Poll::Ready(Ok(()))
        }
    }
    #[tokio::test(start_paused = true)]
    async fn reliable_send_survives_first_deadline_without_dropping_full_sync() {
        let ready = Arc::new(AtomicBool::new(false));
        let wake = Arc::new(Mutex::new(None::<Waker>));
        let sent = Arc::new(Mutex::new(Vec::new()));
        let mut sink = GateSink {
            ready: ready.clone(),
            wake: wake.clone(),
            sent: sent.clone(),
        };
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(15)).await;
            ready.store(true, Ordering::Release);
            if let Some(waker) = wake.lock().unwrap().take() {
                waker.wake();
            }
        });
        let config = crate::Config {
            write_timeout: Duration::from_millis(10),
            max_write_timeouts: 2,
            ..crate::Config::default()
        };
        assert_eq!(
            write_message(&mut sink, Message::Text("full_sync".into()), &config).await,
            Ok(())
        );
        assert_eq!(sent.lock().unwrap().len(), 1);
        assert_eq!(sent.lock().unwrap()[0], Message::Text("full_sync".into()));
    }
}
