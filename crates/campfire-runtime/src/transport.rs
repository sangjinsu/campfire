use crate::{
    Campfire,
    metrics::inc,
    output::Output,
    protocol::{self, Command, Error, ResponseCache},
    rate::Bucket,
    session::TokenHash,
};
use axum::{
    Json,
    extract::{
        ConnectInfo, Path, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    net::SocketAddr,
    sync::{Arc, atomic::AtomicBool},
};
use tokio::{
    sync::{OwnedSemaphorePermit, mpsc, watch},
    time::Instant,
};

pub(crate) async fn health() -> Json<Value> {
    Json(json!({"status":"ok"}))
}
pub(crate) async fn guest(
    State(runtime): State<Campfire>,
    Path(game): Path<String>,
    source: Result<ConnectInfo<SocketAddr>, axum::extract::rejection::ExtensionRejection>,
) -> Response {
    // Proxies must provide a trusted ConnectInfo implementation; untrusted forwarded headers are ignored.
    let source = source.map_or_else(
        |_| "unknown".into(),
        |ConnectInfo(address)| address.ip().to_string(),
    );
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
    let source = source.map_or_else(
        |_| "unknown".into(),
        |ConnectInfo(address)| address.ip().to_string(),
    );
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
    // A bounded transport safety ceiling allows modest application-limit violations to be counted.
    let ceiling = runtime.inner.config.max_frame_bytes.saturating_mul(4);
    ws.max_frame_size(ceiling)
        .max_message_size(ceiling)
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
    unauthenticated: OwnedSemaphorePermit,
) {
    if *runtime.inner.shutdown.borrow() {
        return;
    }
    let config = runtime.inner.config.clone();
    let (reliable, mut reliable_rx) = mpsc::channel(config.reliable_capacity);
    let (latest, mut latest_rx) = watch::channel::<Option<Value>>(None);
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
    let writer = tokio::spawn(async move {
        let mut last_snapshot: Option<(String, u64)> = None;
        loop {
            if *global_stop.borrow() {
                break;
            }
            let next = tokio::select! {biased;
                value=reliable_rx.recv()=>value,
                _=writer_stop.changed()=>None,
                _=global_stop.changed()=>None,
                changed=latest_rx.changed()=> {
                    if changed.is_err(){break;}
                    let value=latest_rx.borrow_and_update().clone();
                    if value.is_none(){continue;}
                    value
                }
            };
            let Some(value) = next else {
                break;
            };
            let kind = value["type"].as_str().unwrap_or_default();
            let mut next_snapshot = None;
            if matches!(kind, "room.entered" | "room.resumed" | "snapshot") {
                let instance = value["room_instance_id"].as_str().unwrap_or_default();
                let sequence = value["snapshot_seq"].as_u64().unwrap_or(0);
                if kind == "snapshot"
                    && last_snapshot
                        .as_ref()
                        .is_none_or(|(current, last)| current != instance || sequence <= *last)
                {
                    continue;
                }
                next_snapshot = Some((instance.to_owned(), sequence));
            }
            match write_message(
                &mut sink,
                Message::Text(value.to_string().into()),
                &writer_config,
            )
            .await
            {
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
    let mut reader_stop = output.stop.subscribe();
    let reader = tokio::spawn(async move {
        let mut first = true;
        let mut violations = 0;
        let mut rate = Bucket::new(reader_config.input_burst);
        loop {
            let message = tokio::select! {biased;_=reader_stop.changed()=>break,message=stream.next()=>message};
            let Some(Ok(message)) = message else {
                break;
            };
            let text = match message {
                Message::Text(text) => text,
                Message::Close(_) => break,
                Message::Ping(_) | Message::Pong(_) => continue,
                Message::Binary(_) => {
                    reader_output.send(
                        Error::new("INVALID_MESSAGE", "binary messages are not supported")
                            .value(None),
                    );
                    if first {
                        break;
                    }
                    continue;
                }
            };
            if text.len() > reader_config.max_frame_bytes {
                inc(&reader_output.metrics.abusive_frames);
                violations += 1;
                reader_output.send(
                    Error::new("FRAME_TOO_LARGE", "JSON frame exceeds configured limit")
                        .value(None),
                );
                if first || violations >= reader_config.max_abuse_violations {
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
                    reader_output.send(
                        Error::new(code, "invalid protocol envelope or command schema")
                            .value(id.as_deref()),
                    );
                    if first {
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
            if matches!(
                request.command,
                Command::Input { .. } | Command::Execute { .. }
            ) && !rate.take(
                reader_config.input_rate_per_second,
                reader_config.input_burst,
                Instant::now(),
            ) {
                inc(&reader_output.metrics.abusive_inputs);
                violations += 1;
                reader_output.send(
                    Error::new("RATE_LIMITED", "connection input rate exceeded")
                        .value(Some(&request.id)),
                );
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
                    output.send(response);
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
            output.send(response.clone());
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
                    output.send(response.clone());
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
                if !already_enqueued {
                    output.send(response.clone());
                }
                cache.insert(&request, response);
                if matches!(request.command, Command::Logout) {
                    break;
                }
            }
            Err(error) => {
                let response = error.value(Some(&request.id));
                output.send(response.clone());
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
