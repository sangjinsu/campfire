//! Real HTTP/WebSocket checks independent of the optional arena game.
use campfire_runtime::{
    AccountAuthenticator, AccountGrant, AccountIdentity, AuthError, Campfire, Config, Game,
    GameContext, GameError, GameModule, PlayerIdentity,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap, future::Future, net::SocketAddr, pin::Pin, sync::Arc, time::Duration,
};
use tokio::{
    net::TcpListener,
    task::JoinHandle,
    time::{sleep, timeout},
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
const ALICE_SECRET: &str = "alice-private-inventory-marker";
const BOB_SECRET: &str = "bob-private-inventory-marker";

struct TestAuthenticator {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
impl AccountAuthenticator for TestAuthenticator {
    fn authenticate<'a>(
        &'a self,
        _game: &'a str,
        credential: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<AccountGrant, AuthError>> + Send + 'a>> {
        Box::pin(async move {
            if credential == "blocked" {
                self.entered.notify_one();
                self.release.notified().await;
            }
            if credential == "slow" {
                sleep(Duration::from_millis(250)).await;
            }
            if credential == "denied-sensitive-credential" {
                return Err(AuthError::InvalidCredential);
            }
            let (issuer, subject, secret) = match credential {
                "bob" => ("test-issuer", "private-subject-bob", BOB_SECRET),
                "other-issuer" => (
                    "other-private-issuer",
                    "private-subject-alice",
                    ALICE_SECRET,
                ),
                "alice" | "expired" | "slow" | "blocked" => {
                    ("test-issuer", "private-subject-alice", ALICE_SECRET)
                }
                _ => return Err(AuthError::InvalidCredential),
            };
            Ok(AccountGrant {
                identity: AccountIdentity {
                    issuer: issuer.into(),
                    subject: subject.into(),
                    claims: json!({"inventory_secret": secret}),
                },
                valid_for: if credential == "expired" {
                    Duration::ZERO
                } else {
                    Duration::from_secs(60)
                },
            })
        })
    }
}

struct CounterModule;
#[derive(Default)]
struct Counter {
    values: Vec<u64>,
    secrets: BTreeMap<String, String>,
}
impl GameModule for CounterModule {
    fn create(&self, _: &Value, _: &GameContext) -> Result<Box<dyn Game>, GameError> {
        Ok(Box::new(Counter::default()))
    }
}
impl Game for Counter {
    fn player_joined(&mut self, id: &str) {
        self.secrets
            .insert(id.into(), format!("guest-private-{id}"));
    }
    fn player_joined_with_identity(&mut self, id: &str, identity: &PlayerIdentity) {
        self.player_joined(id);
        self.player_identity_changed(id, identity);
    }
    fn player_identity_changed(&mut self, id: &str, identity: &PlayerIdentity) {
        if let PlayerIdentity::Account(account) = identity {
            self.secrets.insert(
                id.into(),
                account.claims["inventory_secret"].as_str().unwrap().into(),
            );
        }
    }
    fn player_left(&mut self, id: &str) {
        self.secrets.remove(id);
    }
    fn player_connection_changed(&mut self, _: &str, _: bool) {}
    fn validate_input(&self, _: &str, _: &Value) -> Result<(), GameError> {
        Ok(())
    }
    fn apply_input(&mut self, _: &str, _: &Value) {}
    fn validate_command(&self, _: &str, payload: &Value) -> Result<(), GameError> {
        if payload["reject"] == true {
            return Err(GameError::new("REJECTED", "test validation rejection"));
        }
        if payload["value"].as_u64().is_none() {
            return Err(GameError::new("INVALID_VALUE", "value must be unsigned"));
        }
        Ok(())
    }
    fn apply_command(&mut self, id: &str, payload: &Value) -> Result<Value, GameError> {
        if payload["fail"] == true {
            return Err(GameError::new("GAME_FAILED", "test application rejection"));
        }
        self.values.push(payload["value"].as_u64().unwrap());
        Ok(json!({"applied": self.values.len(), "secret": self.secrets[id]}))
    }
    fn private_snapshot(&self, id: &str) -> Option<Value> {
        self.secrets.get(id).map(|secret| json!({"secret": secret}))
    }
    fn tick(&mut self, _: Duration) {}
    fn snapshot(&self) -> Value {
        json!({"values": self.values})
    }
}

struct Server {
    address: SocketAddr,
    task: JoinHandle<()>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
impl Server {
    async fn new(config: Config, accounts: bool) -> Self {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let runtime = if accounts {
            Campfire::with_authenticator(
                config,
                Arc::new(TestAuthenticator {
                    entered: entered.clone(),
                    release: release.clone(),
                }),
            )
        } else {
            Campfire::new(config)
        }
        .unwrap();
        runtime.register_game("a", CounterModule).unwrap();
        runtime.register_game("b", CounterModule).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                runtime
                    .router()
                    .into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        Self {
            address,
            task,
            entered,
            release,
        }
    }
    fn endpoint(&self, game: &str, route: &str) -> String {
        format!("http://{}/v1/games/{game}/{route}", self.address)
    }
    async fn account_response(&self, game: &str, credential: &str) -> reqwest::Response {
        reqwest::Client::new()
            .post(self.endpoint(game, "sessions"))
            .json(&json!({"credential": credential}))
            .send()
            .await
            .unwrap()
    }
    async fn account(&self, game: &str, credential: &str) -> Value {
        let response = self.account_response(game, credential).await;
        assert_eq!(response.status(), 201);
        let value: Value = response.json().await.unwrap();
        assert!(!value.to_string().contains("private-subject"));
        assert!(!value.to_string().contains(ALICE_SECRET));
        value
    }
    async fn guest(&self, game: &str) -> Value {
        reqwest::Client::new()
            .post(self.endpoint(game, "guests"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
    async fn socket(&self, game: &str) -> Client {
        let socket = connect_async(format!("ws://{}/v1/games/{game}/ws", self.address))
            .await
            .unwrap()
            .0;
        Client {
            socket,
            next_id: 0,
            room: Value::Null,
            forbidden: vec![],
            observed: vec![],
        }
    }
    async fn client(&self, game: &str, session: &Value, forbidden: &[&str]) -> Client {
        let mut client = self.socket(game).await;
        client.forbidden = forbidden.iter().map(|s| (*s).into()).collect();
        let reply = client
            .call("session.authenticate", json!({"token": session["token"]}))
            .await;
        assert_eq!(reply["type"], "session.authenticated", "{reply}");
        client
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
struct Client {
    socket: Socket,
    next_id: u64,
    room: Value,
    forbidden: Vec<String>,
    observed: Vec<Value>,
}
impl Client {
    async fn send(&mut self, kind: &str, mut body: Value) -> String {
        self.next_id += 1;
        let id = format!("r-{}", self.next_id);
        body["v"] = json!(1);
        body["type"] = json!(kind);
        body["request_id"] = json!(id);
        self.socket
            .send(Message::Text(body.to_string().into()))
            .await
            .unwrap();
        id
    }
    async fn recv(&mut self) -> Value {
        timeout(Duration::from_secs(3), async {
            loop {
                match self
                    .socket
                    .next()
                    .await
                    .expect("socket ended")
                    .expect("websocket error")
                {
                    Message::Text(text) => {
                        // Check every packet, including those skipped while matching request IDs.
                        for secret in &self.forbidden {
                            assert!(!text.contains(secret), "recipient privacy violation");
                        }
                        for identity in [
                            "private-subject-alice",
                            "private-subject-bob",
                            "other-private-issuer",
                            "inventory_secret",
                        ] {
                            assert!(!text.contains(identity), "account metadata leaked");
                        }
                        let value = serde_json::from_str(&text).unwrap();
                        self.observed.push(value);
                        return self.observed.last().unwrap().clone();
                    }
                    Message::Ping(_) | Message::Pong(_) => {}
                    other => panic!("unexpected websocket frame: {other:?}"),
                }
            }
        })
        .await
        .expect("response deadline")
    }
    async fn response(&mut self, request: &str) -> Value {
        loop {
            let value = self.recv().await;
            if value["request_id"] == request {
                return value;
            }
        }
    }
    async fn call(&mut self, kind: &str, body: Value) -> Value {
        let id = self.send(kind, body).await;
        self.response(&id).await
    }
    async fn enter(&mut self, room: Option<&Value>) -> Value {
        let body = room
            .map(room_ids)
            .unwrap_or_else(|| json!({"settings": {}}));
        let reply = self
            .call(
                if room.is_some() {
                    "room.join"
                } else {
                    "room.create"
                },
                body,
            )
            .await;
        assert_eq!(reply["type"], "room.entered", "{reply}");
        assert_eq!(reply["last_processed_command_seq"], 0);
        assert_eq!(reply["command_results"], json!([]));
        self.room = reply.clone();
        reply
    }
    fn command_fields(&self, seq: u64, payload: Value) -> Value {
        let mut value = room_ids(&self.room);
        value["command_seq"] = json!(seq);
        value["payload"] = payload;
        value
    }
    async fn command(&mut self, seq: u64, payload: Value) -> Value {
        self.call("game.command", self.command_fields(seq, payload))
            .await
    }
    async fn snapshot_ack(&mut self, seq: u64) -> Value {
        loop {
            let value = self.snapshot().await;
            if value["last_processed_command_seq"].as_u64().unwrap() >= seq {
                return value;
            }
        }
    }
    async fn snapshot(&mut self) -> Value {
        loop {
            let value = self.recv().await;
            if value["type"] == "snapshot" {
                return value;
            }
        }
    }
}
fn room_ids(room: &Value) -> Value {
    json!({"room_id": room["room_id"], "room_instance_id": room["room_instance_id"]})
}
fn code(value: &Value) -> &str {
    value["code"]
        .as_str()
        .or_else(|| value["error"]["code"].as_str())
        .unwrap_or("")
}
fn config() -> Config {
    Config {
        guest_burst: 1000,
        global_guest_burst: 2000,
        ..Config::default()
    }
}

#[tokio::test]
async fn account_endpoint_rejects_disabled_invalid_expired_and_client_identity() {
    let disabled = Server::new(config(), false).await;
    assert_eq!(disabled.account_response("a", "alice").await.status(), 501);
    let server = Server::new(config(), true).await;
    for credential in ["denied-sensitive-credential", "expired"] {
        let response = server.account_response("a", credential).await;
        assert_eq!(
            response.status(),
            if credential == "expired" { 503 } else { 401 }
        );
        let body = response.text().await.unwrap();
        assert!(!body.contains(credential));
        assert!(!body.contains("provider-sensitive-error"));
    }
    for body in [
        json!({"credential":"alice", "subject":"forged"}),
        json!({"credential":"alice", "claims":{}}),
        json!({"credential":"alice", "player_id":"forged"}),
    ] {
        let response = reqwest::Client::new()
            .post(server.endpoint("a", "sessions"))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
    }
    let malformed = reqwest::Client::new()
        .post(server.endpoint("a", "sessions"))
        .header("content-type", "application/json")
        .body("{\"credential\":\"sensitive-malformed")
        .send()
        .await
        .unwrap();
    assert_eq!(malformed.status(), 400);
    assert!(
        !malformed
            .text()
            .await
            .unwrap()
            .contains("sensitive-malformed")
    );
    let oversized = reqwest::Client::new()
        .post(server.endpoint("a", "sessions"))
        .json(&json!({"credential": "sensitive-oversized".repeat(2000)}))
        .send()
        .await
        .unwrap();
    assert_eq!(oversized.status(), 413);
    assert!(
        !oversized
            .text()
            .await
            .unwrap()
            .contains("sensitive-oversized")
    );
    let _guest = server.guest("a").await;
}

#[tokio::test]
async fn account_keys_are_scoped_by_game_and_issuer_and_tokens_cannot_cross_games() {
    let server = Server::new(config(), true).await;
    let alice = server.account("a", "alice").await;
    let other_game = server.account("b", "alice").await;
    let other_issuer = server.account("a", "other-issuer").await;
    assert_ne!(alice["player_id"], other_game["player_id"]);
    assert_ne!(alice["player_id"], other_issuer["player_id"]);
    assert!(!alice["player_id"].is_null());
    let mut wrong = server.socket("b").await;
    assert_eq!(
        code(
            &wrong
                .call("session.authenticate", json!({"token":alice["token"]}))
                .await
        ),
        "GAME_MISMATCH"
    );
    let mut valid = server.client("a", &alice, &[BOB_SECRET]).await;
    assert_eq!(
        valid.enter(None).await["private_state"]["secret"],
        ALICE_SECRET
    );
}

#[tokio::test]
async fn command_fifo_replay_conflict_gap_failures_and_resume_preserve_exact_mutations() {
    let server = Server::new(
        Config {
            tick_hz: 1,
            snapshot_hz: 1,
            ..config()
        },
        true,
    )
    .await;
    let session = server.account("a", "alice").await;
    let mut client = server.client("a", &session, &[BOB_SECRET]).await;
    let room = client.enter(None).await;
    let first = client
        .send(
            "game.command",
            client.command_fields(1, json!({"value":11})),
        )
        .await;
    let second = client
        .send(
            "game.command",
            client.command_fields(2, json!({"value":22})),
        )
        .await;
    // Both terminal results must arrive before the next one-second game tick.
    timeout(Duration::from_millis(500), async {
        assert_eq!(client.response(&first).await["type"], "game.command.result");
        assert_eq!(
            client.response(&second).await["type"],
            "game.command.result"
        );
    })
    .await
    .expect("FIFO commands waited for a game tick");
    let duplicate = client.command(1, json!({"value":11})).await;
    assert_eq!(duplicate["type"], "game.command.result");
    assert_eq!(
        code(&client.command(1, json!({"value":99})).await),
        "COMMAND_SEQUENCE_CONFLICT"
    );
    assert_eq!(
        code(&client.command(4, json!({"value":44})).await),
        "COMMAND_SEQUENCE_GAP"
    );
    let rejected = client.command(3, json!({"reject":true})).await;
    assert_eq!(rejected["type"], "game.command.result");
    assert_eq!(code(&rejected), "REJECTED");
    let failed = client.command(4, json!({"value":44,"fail":true})).await;
    assert_eq!(failed["type"], "game.command.result");
    assert_eq!(code(&failed), "GAME_FAILED");
    assert_eq!(
        client.command(5, json!({"value":55})).await["type"],
        "game.command.result"
    );
    let snapshot = client.snapshot_ack(5).await;
    assert_eq!(snapshot["state"]["values"], json!([11, 22, 55]));
    assert_eq!(snapshot["last_processed_command_seq"], 5);
    drop(client);
    let mut resumed = server.client("a", &session, &[BOB_SECRET]).await;
    resumed.room = room.clone();
    let full = resumed.call("resume", room_ids(&room)).await;
    assert_eq!(full["type"], "room.resumed");
    assert_eq!(full["last_processed_command_seq"], 5);
    assert_eq!(full["command_results"].as_array().unwrap().len(), 5);
    assert_eq!(
        resumed.command(2, json!({"value":22})).await["type"],
        "game.command.result"
    );
    let snapshot = resumed.snapshot_ack(5).await;
    assert_eq!(snapshot["state"]["values"], json!([11, 22, 55]));
}

#[tokio::test]
async fn account_rotation_closes_old_socket_preserves_room_and_private_state_stays_recipient_local()
{
    let server = Server::new(config(), true).await;
    let alice = server.account("a", "alice").await;
    let bob = server.account("a", "bob").await;
    let mut a = server.client("a", &alice, &[BOB_SECRET]).await;
    let mut b = server.client("a", &bob, &[ALICE_SECRET]).await;
    let room = a.enter(None).await;
    b.enter(Some(&room)).await;
    assert_eq!(
        a.command(1, json!({"value":1})).await["type"],
        "game.command.result"
    );
    assert_eq!(
        b.command(1, json!({"value":2})).await["type"],
        "game.command.result"
    );
    assert_eq!(a.snapshot().await["private_state"]["secret"], ALICE_SECRET);
    assert_eq!(b.snapshot().await["private_state"]["secret"], BOB_SECRET);
    let replacement = server.account("a", "alice").await;
    assert_eq!(alice["player_id"], replacement["player_id"]);
    assert_ne!(alice["token"], replacement["token"]);
    timeout(Duration::from_secs(2), async {
        while let Some(frame) = a.socket.next().await {
            match frame {
                Ok(Message::Close(_)) | Err(_) => break,
                Ok(Message::Text(text)) => {
                    assert!(!text.contains(BOB_SECRET));
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    let mut old = server.socket("a").await;
    assert_eq!(
        code(
            &old.call("session.authenticate", json!({"token":alice["token"]}))
                .await
        ),
        "UNAUTHENTICATED"
    );
    let mut a = server.client("a", &replacement, &[BOB_SECRET]).await;
    a.room = room.clone();
    let resumed = a.call("resume", room_ids(&room)).await;
    assert_eq!(resumed["type"], "room.resumed");
    assert_eq!(resumed["private_state"]["secret"], ALICE_SECRET);
    assert_eq!(resumed["last_processed_command_seq"], 1);
    assert_eq!(
        a.command(1, json!({"value":1})).await["type"],
        "game.command.result"
    );
    assert_eq!(
        a.call("room.leave", room_ids(&room)).await["type"],
        "room.left"
    );
    let joined = a.enter(Some(&room)).await;
    assert_eq!(joined["command_results"], json!([]));
    assert_eq!(
        a.command(1, json!({"value":3})).await["type"],
        "game.command.result"
    );
    assert_eq!(a.snapshot_ack(1).await["state"]["values"], json!([1, 2, 3]));
    b.snapshot().await;
}

#[tokio::test]
async fn concurrent_account_logins_leave_only_the_last_issued_token_valid() {
    let server = Server::new(config(), true).await;
    let (first, second) = tokio::join!(server.account("a", "alice"), server.account("a", "alice"));
    assert_eq!(first["player_id"], second["player_id"]);
    let mut valid = 0;
    for session in [first, second] {
        let mut socket = server.socket("a").await;
        let response = socket
            .call("session.authenticate", json!({"token":session["token"]}))
            .await;
        if response["type"] == "session.authenticated" {
            valid += 1;
        } else {
            assert_eq!(code(&response), "UNAUTHENTICATED");
        }
    }
    assert_eq!(valid, 1);
}

#[tokio::test]
async fn pending_host_verification_does_not_block_health_or_guest_issuance() {
    let server = Server::new(config(), true).await;
    let slow = server.account_response("a", "blocked");
    let concurrent = async {
        server.entered.notified().await;
        timeout(Duration::from_secs(1), async {
            let response = reqwest::get(format!("http://{}/health", server.address))
                .await
                .unwrap();
            assert!(response.status().is_success());
            let guest = server.guest("a").await;
            assert!(guest["token"].is_string());
        })
        .await
        .expect("host verifier blocked shared runtime state");
        server.release.notify_one();
    };
    let (response, _) = tokio::join!(slow, concurrent);
    assert_eq!(response.status(), 201);
}

#[tokio::test]
async fn bounded_command_history_rejects_evicted_sequences_without_reapplying() {
    let server = Server::new(
        Config {
            request_cache_capacity: 2,
            ..config()
        },
        true,
    )
    .await;
    let session = server.account("a", "alice").await;
    let mut client = server.client("a", &session, &[BOB_SECRET]).await;
    let room = client.enter(None).await;
    for sequence in 1..=3 {
        assert_eq!(
            client.command(sequence, json!({"value":sequence})).await["type"],
            "game.command.result"
        );
    }
    assert_eq!(
        code(&client.command(1, json!({"value":1})).await),
        "STALE_COMMAND"
    );
    assert_eq!(
        code(&client.command(0, json!({"value":0})).await),
        "STALE_COMMAND"
    );
    assert_eq!(
        code(&client.command(u64::MAX, json!({"value":9})).await),
        "COMMAND_SEQUENCE_GAP"
    );
    drop(client);
    let mut resumed = server.client("a", &session, &[BOB_SECRET]).await;
    let full = resumed.call("resume", room_ids(&room)).await;
    assert_eq!(full["command_results"].as_array().unwrap().len(), 2);
    assert_eq!(full["command_results"][0]["command_seq"], 2);
    assert_eq!(full["command_results"][1]["command_seq"], 3);
    assert_eq!(full["state"]["values"], json!([1, 2, 3]));
}

#[tokio::test]
async fn account_verification_timeout_returns_permit_and_cannot_issue_a_session() {
    let server = Server::new(
        Config {
            auth_timeout: Duration::from_millis(500),
            max_unauthenticated_connections: 1,
            ..config()
        },
        true,
    )
    .await;
    let blocked = server.account_response("a", "blocked");
    let exhaustion = async {
        server.entered.notified().await;
        let response = server.account_response("a", "bob").await;
        assert_eq!(response.status(), 503);
        let value: Value = response.json().await.unwrap();
        assert_eq!(code(&value), "SERVER_BUSY");
    };
    let (timed_out, _) = tokio::join!(blocked, exhaustion);
    assert_eq!(timed_out.status(), 504);
    let value: Value = timed_out.json().await.unwrap();
    assert_eq!(code(&value), "AUTH_TIMEOUT");
    // A later request succeeds after the dropped verifier future returns its permit.
    let _valid = server.account("a", "bob").await;
}

#[tokio::test]
async fn command_request_cache_and_stale_instance_cannot_bypass_membership_validation() {
    let server = Server::new(config(), true).await;
    let session = server.account("a", "alice").await;
    let mut client = server.client("a", &session, &[BOB_SECRET]).await;
    let room = client.enter(None).await;
    let bob = server.account("a", "bob").await;
    let mut keeper = server.client("a", &bob, &[ALICE_SECRET]).await;
    keeper.enter(Some(&room)).await;
    let mut stale = client.command_fields(1, json!({"value":9}));
    stale["room_instance_id"] = json!("wrong-instance");
    assert_eq!(
        code(&client.call("game.command", stale).await),
        "STALE_INSTANCE"
    );
    let mut body = client.command_fields(1, json!({"value":7}));
    body["v"] = json!(1);
    body["type"] = json!("game.command");
    body["request_id"] = json!("fixed-command");
    client
        .socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    assert_eq!(
        client.response("fixed-command").await["type"],
        "game.command.result"
    );
    let mut changed = body.clone();
    changed["payload"] = json!({"value":99});
    client
        .socket
        .send(Message::Text(changed.to_string().into()))
        .await
        .unwrap();
    assert_eq!(
        code(&client.response("fixed-command").await),
        "REQUEST_ID_CONFLICT"
    );
    client.call("room.leave", room_ids(&room)).await;
    client
        .socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    assert_eq!(client.response("fixed-command").await["type"], "error");
    client.enter(Some(&room)).await;
    // A new membership may execute seq=1 even if this connection cached the old reply.
    client
        .socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    assert_eq!(
        client.response("fixed-command").await["type"],
        "game.command.result"
    );
    assert_eq!(
        client.snapshot_ack(1).await["state"]["values"],
        json!([7, 7])
    );
}

#[tokio::test]
async fn account_endpoint_preserves_media_type_status_without_echoing_credentials() {
    let server = Server::new(config(), true).await;
    let client = reqwest::Client::new();
    for content_type in [None, Some("text/plain")] {
        let mut request = client
            .post(server.endpoint("a", "sessions"))
            .body(r#"{"credential":"media-sensitive-credential"}"#);
        if let Some(content_type) = content_type {
            request = request.header("content-type", content_type);
        }
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), 415);
        assert!(
            !response
                .text()
                .await
                .unwrap()
                .contains("media-sensitive-credential")
        );
    }
    for body in [
        json!({}),
        json!({"credential":17}),
        json!({"credential":""}),
    ] {
        let response = client
            .post(server.endpoint("a", "sessions"))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
    }
    assert_eq!(
        server
            .account_response("missing-game", "alice")
            .await
            .status(),
        404
    );
}
