use campfire_runtime::{
    Campfire, Config, ConnectionLimits, Game, GameContext, GameError, GameModule, RuntimeOptions,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{net::SocketAddr, time::Duration};
use tokio::{net::TcpListener, task::JoinHandle, time::timeout};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

struct EmptyModule;
struct EmptyGame;
impl GameModule for EmptyModule {
    fn create(&self, _: &Value, _: &GameContext) -> Result<Box<dyn Game>, GameError> {
        Ok(Box::new(EmptyGame))
    }
}
impl Game for EmptyGame {
    fn player_joined(&mut self, _: &str) {}
    fn player_left(&mut self, _: &str) {}
    fn player_connection_changed(&mut self, _: &str, _: bool) {}
    fn validate_input(&self, _: &str, _: &Value) -> Result<(), GameError> {
        Ok(())
    }
    fn apply_input(&mut self, _: &str, _: &Value) {}
    fn tick(&mut self, _: Duration) {}
    fn snapshot(&self) -> Value {
        json!({})
    }
}

struct TestServer {
    runtime: Campfire,
    address: SocketAddr,
    task: JoinHandle<()>,
}
impl TestServer {
    async fn new(config: Config) -> Self {
        let runtime = Campfire::new(config).unwrap();
        runtime.register_game("test", EmptyModule).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn({
            let app = runtime.router();
            async move {
                axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .await
                .unwrap();
            }
        });
        Self {
            runtime,
            address,
            task,
        }
    }

    async fn authenticated_socket(&self) -> Socket {
        let guest = self.guest().await;
        self.authenticated_socket_for(&guest["token"]).await
    }

    async fn guest(&self) -> Value {
        reqwest::Client::new()
            .post(format!("http://{}/v1/games/test/guests", self.address))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    async fn authenticated_socket_for(&self, token: &Value) -> Socket {
        let (mut socket, _) = connect_async(format!("ws://{}/v1/games/test/ws", self.address))
            .await
            .unwrap();
        send(
            &mut socket,
            json!({"v":1,"type":"session.authenticate","request_id":"auth","token":token}),
        )
        .await;
        assert_eq!(recv(&mut socket).await["type"], "session.authenticated");
        socket
    }

    async fn with_limits(config: Config, limits: ConnectionLimits) -> Self {
        let runtime = Campfire::new_with_options(
            config,
            RuntimeOptions {
                connection_limits: limits,
            },
        )
        .unwrap();
        runtime.register_game("test", EmptyModule).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn({
            let app = runtime.router();
            async move {
                axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .await
                .unwrap();
            }
        });
        Self {
            runtime,
            address,
            task,
        }
    }
}
impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn send(socket: &mut Socket, message: Value) {
    socket
        .send(Message::Text(message.to_string().into()))
        .await
        .unwrap();
}
async fn recv(socket: &mut Socket) -> Value {
    timeout(Duration::from_secs(3), async {
        loop {
            match socket.next().await.unwrap().unwrap() {
                Message::Text(text) => return serde_json::from_str(&text).unwrap(),
                Message::Ping(_) | Message::Pong(_) => {}
                other => panic!("unexpected websocket frame: {other:?}"),
            }
        }
    })
    .await
    .unwrap()
}

fn list(id: impl Into<String>) -> Value {
    json!({"v":1,"type":"room.list","request_id":id.into(),"cursor":null,"limit":1})
}

async fn response_for(socket: &mut Socket, id: &str) -> Value {
    loop {
        let response = recv(socket).await;
        if response["request_id"] == id {
            return response;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hardening_control_flood_is_limited() {
    let server = TestServer::new(Config::default()).await;
    let mut socket = server.authenticated_socket().await;
    for index in 0..24 {
        send(&mut socket, list(format!("list-{index}"))).await;
    }
    let mut limited = false;
    for _ in 0..24 {
        match timeout(Duration::from_secs(3), socket.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                let response: Value = serde_json::from_str(&text).unwrap();
                limited |= response["code"] == "RATE_LIMITED";
            }
            Ok(Some(Ok(Message::Close(_))) | None) => break,
            Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => continue,
            Ok(Some(Err(_))) => break,
            Ok(Some(Ok(other))) => panic!("unexpected websocket frame: {other:?}"),
            Err(_) => break,
        }
    }
    assert!(limited, "control flood produced no RATE_LIMITED response");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hardening_malformed_frames_close_within_violation_limit() {
    let server = TestServer::new(Config::default()).await;
    let mut socket = server.authenticated_socket().await;
    for _ in 0..5 {
        socket.send(Message::Text("{".into())).await.unwrap();
    }
    let mut closed = false;
    for _ in 0..=5 {
        match timeout(Duration::from_millis(500), socket.next()).await {
            Ok(Some(Ok(Message::Text(_)))) => {}
            Ok(Some(Ok(Message::Close(_))) | None | Some(Err(_))) => {
                closed = true;
                break;
            }
            Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => {}
            Ok(Some(Ok(other))) => panic!("unexpected websocket frame: {other:?}"),
            Err(_) => break,
        }
    }
    assert!(closed, "malformed frame flood did not close the socket");
    assert_eq!(server.runtime.metrics().abusive_frames, 5);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hardening_replay_consumes_control_budget() {
    let server = TestServer::new(Config::default()).await;
    let mut socket = server.authenticated_socket().await;
    for _ in 0..24 {
        send(&mut socket, list("repeated-list")).await;
    }
    let mut limited = false;
    for _ in 0..24 {
        match timeout(Duration::from_secs(3), socket.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                let response: Value = serde_json::from_str(&text).unwrap();
                limited |= response["code"] == "RATE_LIMITED";
            }
            Ok(Some(Ok(Message::Close(_))) | None) => break,
            Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => continue,
            Ok(Some(Err(_))) => break,
            Ok(Some(Ok(other))) => panic!("unexpected websocket frame: {other:?}"),
            Err(_) => break,
        }
    }
    assert!(limited, "cached replay bypassed the control budget");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hardening_input_and_control_budgets_are_independent() {
    let config = Config {
        input_rate_per_second: 1,
        input_burst: 1,
        ..Config::default()
    };
    let server = TestServer::with_limits(
        config,
        ConnectionLimits {
            control_rate_per_second: 1,
            control_burst: 2,
            invalid_rate_per_second: 2,
            invalid_burst: 4,
        },
    )
    .await;
    let mut socket = server.authenticated_socket().await;
    send(&mut socket, list("list-1")).await;
    assert_eq!(
        response_for(&mut socket, "list-1").await["type"],
        "room.listed"
    );
    send(&mut socket, list("list-2")).await;
    assert_eq!(
        response_for(&mut socket, "list-2").await["code"],
        "RATE_LIMITED"
    );

    let input = |id: &str, sequence| json!({"v":1,"type":"game.input","request_id":id,"room_id":"missing","room_instance_id":"missing","input_seq":sequence,"payload":{}});
    send(&mut socket, input("input-1", 1)).await;
    assert_ne!(
        response_for(&mut socket, "input-1").await["code"],
        "RATE_LIMITED"
    );
    send(&mut socket, input("input-2", 2)).await;
    assert_eq!(
        response_for(&mut socket, "input-2").await["code"],
        "RATE_LIMITED"
    );

    let mut other = server.authenticated_socket().await;
    send(&mut other, list("other-list")).await;
    assert_eq!(
        response_for(&mut other, "other-list").await["type"],
        "room.listed"
    );
}

#[test]
fn hardening_options_reject_zero_and_overflow_without_changing_legacy_config() {
    assert!(Campfire::new(Config::default()).is_ok());
    for value in [0, u32::MAX] {
        let limits = ConnectionLimits {
            control_burst: value,
            ..ConnectionLimits::default()
        };
        assert!(
            Campfire::new_with_options(
                Config::default(),
                RuntimeOptions {
                    connection_limits: limits,
                },
            )
            .is_err()
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hardening_normal_create_and_cached_list_remain_available() {
    let server = TestServer::new(Config::default()).await;
    let mut socket = server.authenticated_socket().await;
    send(
        &mut socket,
        json!({"v":1,"type":"room.create","request_id":"create","settings":{}}),
    )
    .await;
    let entered = response_for(&mut socket, "create").await;
    assert_eq!(entered["type"], "room.entered");
    send(&mut socket, list("list")).await;
    let first = response_for(&mut socket, "list").await;
    assert_eq!(first["type"], "room.listed");
    send(&mut socket, list("list")).await;
    assert_eq!(response_for(&mut socket, "list").await, first);
    send(&mut socket, json!({"v":1,"type":"room.leave","request_id":"leave","room_id":entered["room_id"],"room_instance_id":entered["room_instance_id"]})).await;
    assert_eq!(
        response_for(&mut socket, "leave").await["type"],
        "room.left"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hardening_invalid_budget_counts_binary_and_closes_early() {
    let server = TestServer::with_limits(
        Config {
            max_abuse_violations: 10,
            ..Config::default()
        },
        ConnectionLimits {
            invalid_rate_per_second: 1,
            invalid_burst: 1,
            ..ConnectionLimits::default()
        },
    )
    .await;
    let mut socket = server.authenticated_socket().await;
    socket
        .send(Message::Binary(vec![1, 2].into()))
        .await
        .unwrap();
    assert_eq!(recv(&mut socket).await["code"], "INVALID_MESSAGE");
    send(&mut socket, list("after-binary")).await;
    assert_eq!(
        response_for(&mut socket, "after-binary").await["type"],
        "room.listed"
    );
    socket.send(Message::Text("{".into())).await.unwrap();
    let mut closed = false;
    for _ in 0..2 {
        match timeout(Duration::from_secs(2), socket.next()).await {
            Ok(Some(Ok(Message::Close(_))) | None | Some(Err(_))) => {
                closed = true;
                break;
            }
            Ok(Some(Ok(Message::Text(_)))) => {}
            other => panic!("unexpected frame while closing invalid connection: {other:?}"),
        }
    }
    assert!(closed);
    assert_eq!(server.runtime.metrics().abusive_frames, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hardening_resume_after_disconnect_uses_control_budget() {
    let server = TestServer::new(Config::default()).await;
    let guest = server.guest().await;
    let mut first = server.authenticated_socket_for(&guest["token"]).await;
    send(
        &mut first,
        json!({"v":1,"type":"room.create","request_id":"create","settings":{}}),
    )
    .await;
    let entered = response_for(&mut first, "create").await;
    assert_eq!(entered["type"], "room.entered");
    first.close(None).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut second = server.authenticated_socket_for(&guest["token"]).await;
    send(
        &mut second,
        json!({"v":1,"type":"resume","request_id":"resume","room_id":entered["room_id"],"room_instance_id":entered["room_instance_id"]}),
    )
    .await;
    assert_eq!(
        response_for(&mut second, "resume").await["type"],
        "room.resumed"
    );
}
