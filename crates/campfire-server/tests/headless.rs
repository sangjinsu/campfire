#![cfg(feature = "arena")]

use campfire_server::{ArenaModule, Campfire, Config};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};
use tokio::{
    net::TcpListener,
    task::JoinHandle,
    time::{sleep, timeout},
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
struct Server {
    runtime: Campfire,
    address: SocketAddr,
    task: JoinHandle<()>,
    config: Config,
}
impl Server {
    async fn new(mut config: Config) -> Self {
        config.guest_burst = 1000;
        config.global_guest_burst = 2000;
        Self::exact(config).await
    }
    async fn exact(config: Config) -> Self {
        let runtime = Campfire::new(config.clone()).unwrap();
        runtime.register_game("a", ArenaModule).unwrap();
        runtime.register_game("b", ArenaModule).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = runtime.router();
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        Self {
            runtime,
            address,
            task,
            config,
        }
    }
    async fn guest(&self, game: &str) -> Value {
        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/games/{game}/guests", self.address))
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "guest response {}",
            response.status()
        );
        response.json().await.unwrap()
    }
    async fn socket(&self, game: &str) -> Socket {
        connect_async(format!("ws://{}/v1/games/{game}/ws", self.address))
            .await
            .unwrap()
            .0
    }
    async fn client(&self, game: &str) -> Client {
        let guest = self.guest(game).await;
        let mut client = Client {
            socket: self.socket(game).await,
            guest,
            room: Value::Null,
            request: 0,
        };
        let token = client.guest["token"].clone();
        let authenticated = client
            .call("session.authenticate", json!({"token":token}))
            .await;
        assert_eq!(
            authenticated["type"], "session.authenticated",
            "{authenticated}"
        );
        assert_eq!(authenticated["can_resume"], false);
        assert!(authenticated["room_id"].is_null());
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
    guest: Value,
    room: Value,
    request: u64,
}
impl Client {
    async fn send(&mut self, message: Value) {
        self.socket
            .send(Message::Text(message.to_string().into()))
            .await
            .unwrap();
    }
    async fn recv(&mut self) -> Value {
        timeout(Duration::from_secs(4), async {
            loop {
                match self
                    .socket
                    .next()
                    .await
                    .expect("socket ended")
                    .expect("websocket failure")
                {
                    Message::Text(text) => return serde_json::from_str(&text).unwrap(),
                    Message::Ping(_) | Message::Pong(_) => {}
                    other => panic!("unexpected frame {other:?}"),
                }
            }
        })
        .await
        .expect("server response deadline")
    }
    async fn response(&mut self, id: &str) -> Value {
        loop {
            let message = self.recv().await;
            if message["request_id"] == id {
                return message;
            }
        }
    }
    async fn call(&mut self, kind: &str, mut fields: Value) -> Value {
        self.request += 1;
        let id = format!("request-{}", self.request);
        fields["v"] = json!(1);
        fields["type"] = json!(kind);
        fields["request_id"] = json!(id);
        self.send(fields).await;
        self.response(&id).await
    }
    async fn enter(&mut self, room: Option<&Value>) -> Value {
        self.request += 1;
        let id = format!("request-{}", self.request);
        let mut command = room.map(room_ids).unwrap_or_else(|| json!({"settings":{}}));
        command["v"] = json!(1);
        command["type"] = json!(if room.is_some() {
            "room.join"
        } else {
            "room.create"
        });
        command["request_id"] = json!(id);
        self.send(command).await;
        // A snapshot before this full sync violates the ordering contract.
        let entered = self.recv().await;
        assert_eq!(entered["request_id"], id);
        assert_eq!(entered["type"], "room.entered", "{entered}");
        for key in [
            "server_tick",
            "snapshot_seq",
            "last_processed_input_seq",
            "state",
        ] {
            assert!(
                !entered[key].is_null(),
                "full sync missing {key}: {entered}"
            );
        }
        self.room = entered.clone();
        entered
    }
    async fn input(&mut self, seq: u64, x: f64, y: f64) {
        self.request += 1;
        let mut message = room_ids(&self.room);
        message["v"] = json!(1);
        message["type"] = json!("game.input");
        message["request_id"] = json!(format!("input-{}", self.request));
        message["input_seq"] = json!(seq);
        message["payload"] = json!({"move_x":x,"move_y":y});
        self.send(message).await;
    }
    async fn snapshot_ack(&mut self, expected: u64) -> Value {
        for _ in 0..120 {
            let value = self.recv().await;
            if value["type"] == "snapshot"
                && value["last_processed_input_seq"].as_u64().unwrap() >= expected
            {
                return value;
            }
        }
        panic!("input ack did not progress");
    }
}
fn room_ids(room: &Value) -> Value {
    json!({"room_id":room["room_id"],"room_instance_id":room["room_instance_id"]})
}
fn assert_error(value: &Value, code: &str) {
    assert_eq!(value["type"], "error", "{value}");
    assert_eq!(value["code"], code, "{value}");
    assert!(value["retryable"].is_boolean());
}
async fn expect_closed(socket: &mut Socket, deadline: Duration) {
    timeout(deadline, async {
        loop {
            match socket.next().await {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            }
        }
    })
    .await
    .expect("server did not close socket");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replay_and_idempotent_leave() {
    let server = Server::new(Config::default()).await;
    let mut client = server.client("a").await;
    let create = json!({"v":1,"type":"room.create","request_id":"create-fixed","settings":{}});
    client.send(create.clone()).await;
    let first = client.response("create-fixed").await;
    assert_eq!(first["type"], "room.entered");
    client.room = first.clone();
    client.send(create.clone()).await;
    assert_eq!(client.response("create-fixed").await, first);
    let mut changed = create;
    changed["settings"] = json!({"speed":3});
    client.send(changed).await;
    let error = client.response("create-fixed").await;
    assert_eq!(error["type"], "error");
    let leave = client.call("room.leave", room_ids(&first)).await;
    assert_eq!(leave["type"], "room.left");
    let again = client.call("room.leave", room_ids(&first)).await;
    assert_eq!(again["type"], "room.left");
    let resume = client.call("resume", room_ids(&first)).await;
    assert_eq!(resume["type"], "error");
    let listed = client
        .call("room.list", json!({"cursor":null,"limit":20}))
        .await;
    assert!(listed["rooms"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn game_isolation_and_stale_instance() {
    let server = Server::new(Config::default()).await;
    let mut b = server.client("b").await;
    let room = b.enter(None).await;
    let mut a = server.client("a").await;
    let listed = a.call("room.list", json!({"cursor":null,"limit":20})).await;
    assert!(listed["rooms"].as_array().unwrap().is_empty());
    for kind in ["room.join", "resume"] {
        let value = a.call(kind, room_ids(&room)).await;
        assert_eq!(value["type"], "error");
    }
    let mut wrong = Client {
        socket: server.socket("b").await,
        guest: Value::Null,
        room: Value::Null,
        request: 0,
    };
    let error = wrong
        .call("session.authenticate", json!({"token":a.guest["token"]}))
        .await;
    assert_error(&error, "GAME_MISMATCH");
    let mut stale = room_ids(&room);
    stale["room_instance_id"] = json!("old-instance");
    let mut another = server.client("b").await;
    assert_error(&another.call("room.join", stale).await, "STALE_INSTANCE");
    let actual = b.call("room.list", json!({"cursor":null,"limit":20})).await;
    assert_eq!(actual["rooms"][0]["members"], 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn last_seat_race_is_atomic() {
    let server = Server::new(Config::default()).await;
    let mut clients = Vec::new();
    let mut owner = server.client("a").await;
    let room = owner.enter(None).await;
    clients.push(owner);
    for _ in 1..15 {
        let mut c = server.client("a").await;
        c.enter(Some(&room)).await;
        clients.push(c);
    }
    let mut c1 = server.client("a").await;
    let mut c2 = server.client("a").await;
    let (r1, r2) = tokio::join!(
        c1.call("room.join", room_ids(&room)),
        c2.call("room.join", room_ids(&room))
    );
    assert_eq!(
        [&r1, &r2]
            .iter()
            .filter(|v| v["type"] == "room.entered")
            .count(),
        1
    );
    assert_error(if r1["type"] == "error" { &r1 } else { &r2 }, "ROOM_FULL");
    let listed = clients[0]
        .call("room.list", json!({"cursor":null,"limit":20}))
        .await;
    assert_eq!(listed["rooms"][0]["members"], 16);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn authentication_deadline_and_invalid_first_message() {
    let config = Config {
        auth_timeout: Duration::from_millis(100),
        ..Config::default()
    };
    let server = Server::new(config).await;
    let mut idle = server.socket("a").await;
    expect_closed(&mut idle, Duration::from_secs(2)).await;
    let mut invalid = server.socket("a").await;
    invalid
        .send(Message::Text(
            json!({"v":1,"type":"room.create","request_id":"unauth","settings":{}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    expect_closed(&mut invalid, Duration::from_secs(2)).await;
    let response = reqwest::Client::new()
        .post(format!("http://{}/v1/games/missing/guests", server.address))
        .send()
        .await
        .unwrap();
    assert!(!response.status().is_success());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn input_validation_and_recipient_ack() {
    let server = Server::new(Config::default()).await;
    let mut a = server.client("a").await;
    let room = a.enter(None).await;
    let mut b = server.client("a").await;
    b.enter(Some(&room)).await;
    a.input(1, 1.0, 0.0).await;
    let first = a.snapshot_ack(1).await;
    assert_eq!(first["last_processed_input_seq"], 1);
    let other = b.snapshot_ack(0).await;
    assert_eq!(other["last_processed_input_seq"], 0);
    for seq in [1, 0] {
        let mut message = room_ids(&room);
        message["input_seq"] = json!(seq);
        message["payload"] = json!({"move_x":1,"move_y":0});
        assert_error(&a.call("game.input", message).await, "STALE_INPUT");
    }
    let mut jump = room_ids(&room);
    jump["input_seq"] = json!(1_000_000);
    jump["payload"] = json!({"move_x":1,"move_y":0});
    assert_eq!(a.call("game.input", jump).await["type"], "error");
    for payload in [
        json!({"move_x":2,"move_y":0}),
        json!({"move_x":"1","move_y":0}),
        json!({"move_x":0,"move_y":0,"x":999}),
        json!({"move_x":0,"move_y":0,"shared_score":999}),
    ] {
        let mut fields = room_ids(&room);
        fields["input_seq"] = json!(2);
        fields["payload"] = payload;
        assert_eq!(a.call("game.input", fields).await["type"], "error");
    }
    a.input(2, 0.0, 1.0).await;
    let second = a.snapshot_ack(2).await;
    assert!(second["snapshot_seq"].as_u64().unwrap() > first["snapshot_seq"].as_u64().unwrap());
    assert!(second["server_tick"].as_u64().unwrap() > first["server_tick"].as_u64().unwrap());
    assert_eq!(second["state"]["shared_score"], 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replacement_requires_resume_and_preserves_membership() {
    let server = Server::new(Config::default()).await;
    let mut old = server.client("a").await;
    let room = old.enter(None).await;
    old.input(1, 1.0, 0.0).await;
    old.snapshot_ack(1).await;
    let mut new = Client {
        socket: server.socket("a").await,
        guest: old.guest.clone(),
        room: room.clone(),
        request: 0,
    };
    let auth = new
        .call("session.authenticate", json!({"token":old.guest["token"]}))
        .await;
    assert_eq!(auth["type"], "session.authenticated");
    assert_eq!(auth["can_resume"], true);
    assert_eq!(auth["room_instance_id"], room["room_instance_id"]);
    let mut fields = room_ids(&room);
    fields["input_seq"] = json!(2);
    fields["payload"] = json!({"move_x":1,"move_y":0});
    assert_eq!(new.call("game.input", fields).await["type"], "error");
    expect_closed(&mut old.socket, Duration::from_secs(2)).await;
    drop(old);
    let resumed = new.call("resume", room_ids(&room)).await;
    assert_eq!(resumed["type"], "room.resumed");
    assert_eq!(resumed["last_processed_input_seq"], 1);
    new.input(2, 0.0, 1.0).await;
    new.snapshot_ack(2).await;
    let listed = new
        .call("room.list", json!({"cursor":null,"limit":20}))
        .await;
    assert_eq!(listed["rooms"][0]["members"], 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn token_expiry_and_logout() {
    let config = Config {
        guest_ttl: Duration::from_millis(250),
        cleanup_interval: Duration::from_millis(10),
        ..Config::default()
    };
    let server = Server::new(config).await;
    let mut client = server.client("a").await;
    client.enter(None).await;
    expect_closed(&mut client.socket, Duration::from_secs(2)).await;
    let mut reconnect = Client {
        socket: server.socket("a").await,
        guest: client.guest.clone(),
        room: Value::Null,
        request: 0,
    };
    assert_eq!(
        reconnect
            .call(
                "session.authenticate",
                json!({"token":client.guest["token"]})
            )
            .await["type"],
        "error"
    );
    let mut logout = server.client("a").await;
    logout.enter(None).await;
    let token = logout.guest["token"].clone();
    assert_eq!(
        logout.call("session.logout", json!({})).await["type"],
        "session.logged_out"
    );
    expect_closed(&mut logout.socket, Duration::from_secs(2)).await;
    let mut revoked = Client {
        socket: server.socket("a").await,
        guest: Value::Null,
        room: Value::Null,
        request: 0,
    };
    assert_eq!(
        revoked
            .call("session.authenticate", json!({"token":token}))
            .await["type"],
        "error"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real 30 second reservation boundary"]
async fn reconnect_grace_expires() {
    let server = Server::new(Config::default()).await;
    let mut first = server.client("a").await;
    let room = first.enter(None).await;
    let guest = first.guest.clone();
    drop(first);
    sleep(Duration::from_millis(100)).await;
    let mut restored = Client {
        socket: server.socket("a").await,
        guest: guest.clone(),
        room: room.clone(),
        request: 0,
    };
    let auth = restored
        .call("session.authenticate", json!({"token":guest["token"]}))
        .await;
    assert_eq!(auth["can_resume"], true);
    assert_eq!(
        restored.call("resume", room_ids(&room)).await["type"],
        "room.resumed"
    );
    drop(restored);
    sleep(Duration::from_secs(31)).await;
    let mut expired = Client {
        socket: server.socket("a").await,
        guest: guest.clone(),
        room: room.clone(),
        request: 0,
    };
    let auth = expired
        .call("session.authenticate", json!({"token":guest["token"]}))
        .await;
    assert_eq!(auth["can_resume"], false);
    assert_eq!(
        expired.call("resume", room_ids(&room)).await["type"],
        "error"
    );
}

fn build_metadata() -> Value {
    use sha2::{Digest, Sha256};
    fn sources(directory: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                sources(&path, files);
            } else if path.extension().is_some_and(|v| v == "rs")
                || path.file_name().is_some_and(|v| v == "Cargo.toml")
            {
                files.push(path);
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = vec![root.join("Cargo.toml"), root.join("Cargo.lock")];
    sources(&root.join("crates"), &mut files);
    files.sort();
    let mut digest = Sha256::new();
    for path in &files {
        digest.update(
            path.strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .as_bytes(),
        );
        digest.update([0]);
        digest.update(std::fs::read(path).unwrap());
    }
    let output = |program: &str, args: &[&str]| {
        std::process::Command::new(program)
            .args(args)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_else(|e| e.to_string())
    };
    json!({"source_sha256":format!("{:x}",digest.finalize()),"source_files":files.len(),"rustc":output("rustc",&["--version"]),"kernel":output("uname",&["-a"]),"os":std::env::consts::OS,"arch":std::env::consts::ARCH})
}
fn percentile(samples: &[f64], fraction: f64) -> f64 {
    let mut values = samples.to_vec();
    values.sort_by(f64::total_cmp);
    values[((values.len().saturating_sub(1)) as f64 * fraction).ceil() as usize]
}
async fn soak(count: usize, seconds: u64) -> Value {
    let server = Server::new(Config::default()).await;
    let mut owner = server.client("a").await;
    let room = owner.enter(None).await;
    let mut clients = vec![owner];
    for _ in 1..count {
        let mut c = server.client("a").await;
        c.enter(Some(&room)).await;
        clients.push(c);
    }
    for client in &mut clients {
        loop {
            let value = client.recv().await;
            if value["type"] == "snapshot"
                && value["state"]["players"].as_array().unwrap().len() == count
            {
                break;
            }
        }
    }
    let start = Instant::now();
    let deadline = start + Duration::from_secs(seconds);
    let mut tasks = Vec::new();
    for (index, mut c) in clients.into_iter().enumerate() {
        tasks.push(tokio::spawn(async move {
            let mut input=tokio::time::interval(Duration::from_millis(100));let mut seq=0u64;let mut snapshots=0u64;let mut last_snapshot=Instant::now();let mut gaps=Vec::new();let mut last_tick=0u64;let mut last_seq=0u64;let mut ack=0u64;
            loop {
                tokio::select! {
                    _=tokio::time::sleep_until(tokio::time::Instant::from_std(deadline))=>break,
                    _=input.tick()=>{seq+=1; let direction=if (seq / 15).is_multiple_of(2) {1.0}else{-1.0}; c.input(seq,if index%2==0 {direction}else{0.0},if index%2==1 {direction}else{0.0}).await;},
                    message=c.recv()=>{
                        if message["type"] == "game.input.accepted" { continue; }
                        assert_eq!(message["type"],"snapshot","unexpected soak message {message}");
                        assert_eq!(message["room_instance_id"],c.room["room_instance_id"]);
                        let tick=message["server_tick"].as_u64().unwrap();let snapshot_seq=message["snapshot_seq"].as_u64().unwrap();let processed=message["last_processed_input_seq"].as_u64().unwrap();
                        assert!(tick>=last_tick);assert!(snapshot_seq>last_seq);assert!(processed>=ack && processed<=seq);
                        if Instant::now()+Duration::from_millis(100)<deadline {assert_eq!(message["state"]["players"].as_array().unwrap().len(),count);}
                        let now=Instant::now();if snapshots>0 {gaps.push(now.duration_since(last_snapshot).as_secs_f64()*1000.0);}last_snapshot=now;snapshots+=1;last_tick=tick;last_seq=snapshot_seq;ack=processed;
                    }
                }
            }
            assert!(snapshots>=seconds*10,"snapshot cadence too low {snapshots}");assert!(ack>=seconds*8,"input processing too low {ack}");
            assert_eq!(c.call("room.leave",room_ids(&c.room)).await["type"],"room.left");
            json!({"client":index,"inputs_sent":seq,"last_processed_input_seq":ack,"snapshots":snapshots,"last_server_tick":last_tick,"snapshot_gap_p50_ms":percentile(&gaps,0.5),"snapshot_gap_p95_ms":percentile(&gaps,0.95),"snapshot_gap_max_ms":percentile(&gaps,1.0)})
        }));
    }
    let mut measured = Vec::new();
    for task in tasks {
        measured.push(task.await.unwrap());
    }
    let elapsed = start.elapsed().as_secs_f64();
    assert!(elapsed >= seconds as f64);
    let result = json!({"scenario":"real_tcp_websocket_soak","clients":count,"minimum_duration_seconds":seconds,"elapsed_seconds":elapsed,"build":build_metadata(),"command":format!("cargo test -p campfire-server --test headless soak_{count}_clients_60_seconds -- --ignored --nocapture"),"environment":{"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"transport":"TCP loopback","clock":"real monotonic"},"config":format!("{:?}",server.config),"metrics":server.runtime.metrics(),"client_metrics":measured});
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../artifacts");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join(format!("soak-{count}.json")),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    server.runtime.shutdown().await;
    result
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real TCP soak, minimum 60 seconds"]
async fn soak_8_clients_60_seconds() {
    println!("{}", soak(8, 60).await);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real TCP soak, minimum 60 seconds"]
async fn soak_16_clients_60_seconds() {
    println!("{}", soak(16, 60).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn caps_and_cleanup() {
    let config = Config {
        max_sessions: 2,
        guest_ttl: Duration::from_millis(250),
        cleanup_interval: Duration::from_millis(10),
        rate_key_ttl: Duration::from_millis(100),
        ..Config::default()
    };
    let server = Server::new(config).await;
    let _one = server.guest("a").await;
    let _two = server.guest("a").await;
    let denied = reqwest::Client::new()
        .post(format!("http://{}/v1/games/a/guests", server.address))
        .send()
        .await
        .unwrap();
    assert!(!denied.status().is_success());
    sleep(Duration::from_millis(350)).await;
    assert_eq!(server.runtime.metrics().active_sessions, 0);
    assert_eq!(server.runtime.metrics().rate_keys, 0);
    let _replacement = server.guest("a").await;
    assert_eq!(server.runtime.metrics().active_sessions, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn abusive_connection_is_isolated() {
    let config = Config {
        input_burst: 3,
        input_rate_per_second: 3,
        max_abuse_violations: 3,
        ..Config::default()
    };
    let server = Server::new(config).await;
    let mut good = server.client("a").await;
    let room = good.enter(None).await;
    let mut abuser = server.client("a").await;
    abuser.enter(Some(&room)).await;
    for seq in 1..=20 {
        if abuser.socket.send(Message::Text(json!({"v":1,"type":"game.input","request_id":format!("burst-{seq}"),"room_id":room["room_id"],"room_instance_id":room["room_instance_id"],"input_seq":seq,"payload":{"move_x":1,"move_y":0}}).to_string().into())).await.is_err(){break;}
    }
    expect_closed(&mut abuser.socket, Duration::from_secs(3)).await;
    good.input(1, 0.0, 1.0).await;
    good.snapshot_ack(1).await;
    assert!(server.runtime.metrics().abusive_inputs >= 3);
    let before = server.runtime.metrics().abusive_inputs;
    sleep(Duration::from_millis(100)).await;
    good.input(2, 0.0, 1.0).await;
    good.snapshot_ack(2).await;
    assert_eq!(server.runtime.metrics().abusive_inputs, before);
    let mut oversized = server.client("a").await;
    for _ in 0..3 {
        if oversized
            .socket
            .send(Message::Text("x".repeat(20_000).into()))
            .await
            .is_err()
        {
            break;
        }
    }
    expect_closed(&mut oversized.socket, Duration::from_secs(3)).await;
    assert!(server.runtime.metrics().abusive_frames >= 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shared_queue_saturation_is_not_abuse() {
    let config = Config {
        room_input_capacity: 1,
        tick_hz: 5,
        snapshot_hz: 5,
        ..Config::default()
    };
    let server = Server::new(config).await;
    let mut owner = server.client("a").await;
    let room = owner.enter(None).await;
    let mut clients = vec![owner];
    for _ in 1..16 {
        let mut c = server.client("a").await;
        c.enter(Some(&room)).await;
        clients.push(c);
    }
    let before = server.runtime.metrics();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(clients.len()));
    let mut tasks = Vec::new();
    for mut c in clients {
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            for seq in 1..=20 {
                c.input(seq, 1.0, 0.0).await;
            }
            let mut busy = 0;
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                let value = c.recv().await;
                if value["code"] == "SERVER_BUSY" {
                    assert_eq!(value["retryable"], true);
                    busy += 1;
                }
            }
            busy
        }));
    }
    let mut busy = 0;
    for task in tasks {
        busy += task.await.unwrap();
    }
    let after = server.runtime.metrics();
    assert!(busy > 0, "shared queue was not saturated");
    assert!(after.shared_queue_overloads > before.shared_queue_overloads);
    assert_eq!(after.abusive_inputs, before.abusive_inputs);
    assert_eq!(after.abusive_frames, before.abusive_frames);
    assert!(after.max_input_queue_depth <= 1);
}

struct PaddedModule {
    bytes: usize,
    panic_on_tick: bool,
}
struct PaddedGame {
    inner: Box<dyn campfire_runtime::Game>,
    padding: String,
    panic_on_tick: bool,
}
impl campfire_runtime::GameModule for PaddedModule {
    fn create(
        &self,
        settings: &Value,
        context: &campfire_runtime::GameContext,
    ) -> Result<Box<dyn campfire_runtime::Game>, campfire_runtime::GameError> {
        Ok(Box::new(PaddedGame {
            inner: ArenaModule.create(settings, context)?,
            padding: "s".repeat(self.bytes),
            panic_on_tick: self.panic_on_tick,
        }))
    }
}
impl campfire_runtime::Game for PaddedGame {
    fn player_joined(&mut self, id: &str) {
        self.inner.player_joined(id)
    }
    fn player_left(&mut self, id: &str) {
        self.inner.player_left(id)
    }
    fn player_connection_changed(&mut self, id: &str, connected: bool) {
        self.inner.player_connection_changed(id, connected)
    }
    fn validate_input(&self, id: &str, payload: &Value) -> Result<(), campfire_runtime::GameError> {
        self.inner.validate_input(id, payload)
    }
    fn apply_input(&mut self, id: &str, payload: &Value) {
        self.inner.apply_input(id, payload)
    }
    fn tick(&mut self, dt: Duration) {
        assert!(
            !self.panic_on_tick,
            "intentional trusted game unwind for supervision test"
        );
        self.inner.tick(dt)
    }
    fn snapshot(&self) -> Value {
        let mut value = self.inner.snapshot();
        value["test_padding"] = json!(self.padding);
        value
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn panic_supervision_cleans_directory() {
    let server = Server::new(Config::default()).await;
    server
        .runtime
        .register_game(
            "panic",
            PaddedModule {
                bytes: 0,
                panic_on_tick: true,
            },
        )
        .unwrap();
    let mut client = server.client("panic").await;
    let room = client.enter(None).await;
    let closed = loop {
        let value = client.recv().await;
        if value["type"] == "room.closed" {
            break value;
        }
    };
    assert_eq!(closed["room_instance_id"], room["room_instance_id"]);
    assert_eq!(
        client.call("resume", room_ids(&room)).await["type"],
        "error"
    );
    let listed = client
        .call("room.list", json!({"cursor":null,"limit":20}))
        .await;
    assert!(listed["rooms"].as_array().unwrap().is_empty());
    assert_eq!(server.runtime.metrics().room_panics, 1);
}

async fn large_snapshot_run(slow: bool) -> Value {
    let config = Config {
        max_frame_bytes: 1024 * 1024,
        write_timeout: Duration::from_millis(100),
        max_write_timeouts: 2,
        ..Config::default()
    };
    let server = Server::new(config).await;
    server
        .runtime
        .register_game(
            "large",
            PaddedModule {
                bytes: 256 * 1024,
                panic_on_tick: false,
            },
        )
        .unwrap();
    println!(
        "slow reader stage={}",
        if slow { "stopped-reader" } else { "baseline" }
    );
    let (start, _) = tokio::sync::watch::channel::<Option<tokio::time::Instant>>(None);
    let mut tasks = Vec::new();
    let mut ready = Vec::new();
    let mut room = None;
    for index in 0..16 {
        let mut client = server.client("large").await;
        let entered = client.enter(room.as_ref()).await;
        if room.is_none() {
            room = Some(entered);
        }
        let mut start = start.subscribe();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        ready.push(ready_rx);
        // Readers run throughout admission so setup cannot create accidental slow clients.
        tasks.push(tokio::spawn(async move {
            let stopped = slow && index == 15;
            let mut ready_tx = Some(ready_tx);
            let mut deadline = None;
            let mut previous = None;
            let mut samples = Vec::new();
            let mut snapshots = 0;
            let mut failure = None;
            loop {
                let end = deadline;
                tokio::select! { biased;
                    changed = start.changed() => {
                        if changed.is_err() { failure=Some("measurement controller closed".to_owned()); break; }
                        deadline = *start.borrow_and_update();
                        previous=None;
                        if stopped {
                            tokio::time::sleep_until(deadline.expect("start signal includes deadline")).await;
                            break;
                        }
                    }
                    _ = async { match end { Some(end) => tokio::time::sleep_until(end).await, None => std::future::pending().await } } => break,
                    frame = client.socket.next() => {
                        match frame {
                            Some(Ok(Message::Text(text))) => {
                                let value:Value=serde_json::from_str(&text).unwrap();
                                if value["type"]!="snapshot" { continue; }
                                if value["state"]["players"].as_array().unwrap().len()==16 && let Some(tx)=ready_tx.take() { let _=tx.send(()); }
                                if deadline.is_some() {
                                    let now=Instant::now();
                                    if let Some(last)=previous { samples.push(now.duration_since(last).as_secs_f64()*1000.0); }
                                    previous=Some(now); snapshots+=1;
                                }
                            }
                            Some(Ok(Message::Ping(_)|Message::Pong(_)))=>{},
                            other=>{ failure=Some(format!("client {index} unexpected frame: {other:?}"));break; }
                        }
                    }
                }
            }
            (client, samples, snapshots, failure)
        }));
    }
    for receiver in ready {
        if !matches!(timeout(Duration::from_secs(5), receiver).await, Ok(Ok(()))) {
            panic!(
                "setup reader never observed all members; slow={slow}; metrics={:?}",
                server.runtime.metrics()
            );
        }
    }
    start.send_replace(Some(tokio::time::Instant::now() + Duration::from_secs(8)));
    let mut held_clients = Vec::new();
    let mut p95 = Vec::new();
    let mut failures = Vec::new();
    for (index, task) in tasks.into_iter().enumerate() {
        let (client, samples, snapshots, failure) = task.await.unwrap();
        held_clients.push(client);
        if let Some(failure) = failure {
            failures.push(failure);
        }
        if !(slow && index == 15) {
            if snapshots <= 50 {
                failures.push(format!(
                    "client {index} only observed {snapshots} snapshots"
                ));
            }
            if !samples.is_empty() {
                p95.push(percentile(&samples, 0.95));
            }
        }
    }
    let metrics = server.runtime.metrics();
    println!("slow reader stage={slow} metrics={metrics:?} failures={failures:?}");
    assert!(
        failures.is_empty(),
        "normal reader failed; slow={slow}; metrics={metrics:?}; failures={failures:?}"
    );
    let disconnects = metrics.write_deadline_disconnects + metrics.reliable_queue_disconnects;
    assert_eq!(
        disconnects,
        u64::from(slow),
        "wrong number of server-isolated connections: {metrics:?}"
    );
    assert_eq!(
        metrics.active_connections,
        if slow { 15 } else { 16 },
        "normal connection was lost: {metrics:?}"
    );
    assert!(metrics.max_reliable_queue_depth <= server.config.reliable_capacity as u64);
    let result = json!({"slow_reader":slow,"snapshot_padding_bytes":256*1024,"normal_clients":p95.len(),"p95_ms":percentile(&p95,0.95),"per_client_p95_ms":p95,"metrics":metrics});
    drop(held_clients);
    server.runtime.shutdown().await;
    result
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real socket backpressure and matched baseline measurement"]
async fn slow_reader_isolated_and_p95() {
    let baseline = large_snapshot_run(false).await;
    let slow = large_snapshot_run(true).await;
    let ratio = slow["p95_ms"].as_f64().unwrap() / baseline["p95_ms"].as_f64().unwrap();
    let result = json!({"scenario":"slow_reader","build":build_metadata(),"command":"cargo test -p campfire-server --test headless slow_reader_isolated_and_p95 -- --ignored --nocapture","config":{"frame_bytes":1048576,"write_timeout_ms":100,"max_write_timeouts":2,"snapshot_hz":15},"baseline":baseline,"slow":slow,"normal_snapshot_p95_ratio":ratio,"threshold":2.0});
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/slow-reader.json");
    std::fs::write(path, serde_json::to_vec_pretty(&result).unwrap()).unwrap();
    println!("{result}");
    assert!(ratio <= 2.0, "normal p95 regression: {ratio}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repeated_authentication_does_not_extend_reservation() {
    let config = Config {
        reconnect_grace: Duration::from_millis(500),
        cleanup_interval: Duration::from_millis(10),
        ..Config::default()
    };
    let server = Server::new(config).await;
    let mut original = server.client("a").await;
    let room = original.enter(None).await;
    let token = original.guest["token"].clone();
    let mut second = Client {
        socket: server.socket("a").await,
        guest: original.guest.clone(),
        room: room.clone(),
        request: 0,
    };
    let first_auth = second
        .call("session.authenticate", json!({"token":token}))
        .await;
    assert_eq!(first_auth["can_resume"], true);
    sleep(Duration::from_millis(250)).await;
    let mut third = Client {
        socket: server.socket("a").await,
        guest: original.guest.clone(),
        room: room.clone(),
        request: 0,
    };
    let next_auth = third
        .call("session.authenticate", json!({"token":token}))
        .await;
    assert_eq!(next_auth["can_resume"], true);
    assert_eq!(
        first_auth["reservation_expires_at"], next_auth["reservation_expires_at"],
        "replacement extended grace"
    );
    sleep(Duration::from_millis(350)).await;
    assert_eq!(third.call("resume", room_ids(&room)).await["type"], "error");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn schema_and_explicit_game_mismatch_are_rejected() {
    let server = Server::new(Config::default()).await;
    let mut c = server.client("a").await;
    for (index, command) in [
        json!({"v":2,"type":"room.list","cursor":null,"limit":10}),
        json!({"v":1,"type":"unknown"}),
        json!({"v":1,"type":"room.list","limit":10}),
        json!({"v":1,"type":"room.list","cursor":null,"limit":"10"}),
    ]
    .into_iter()
    .enumerate()
    {
        let mut command = command;
        command["request_id"] = json!(format!("schema-{index}"));
        c.send(command).await;
        let error = c.recv().await;
        assert_eq!(error["type"], "error");
    }
    for (kind, fields) in [
        ("room.create", json!({"settings":{},"game_id":"b"})),
        ("room.list", json!({"cursor":null,"limit":10,"game_id":"b"})),
        (
            "room.join",
            json!({"room_id":"any","room_instance_id":"any","game_id":"b"}),
        ),
        (
            "resume",
            json!({"room_id":"any","room_instance_id":"any","game_id":"b"}),
        ),
    ] {
        assert_error(&c.call(kind, fields).await, "GAME_MISMATCH");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn connection_and_unauthenticated_caps_refund_permits() {
    let config = Config {
        max_connections: 2,
        max_unauthenticated_connections: 1,
        ..Config::default()
    };
    let server = Server::new(config).await;
    let mut first = server.socket("a").await;
    assert!(
        connect_async(format!("ws://{}/v1/games/a/ws", server.address))
            .await
            .is_err(),
        "unauthenticated cap accepted another socket"
    );
    let guest = server.guest("a").await;
    first
        .send(Message::Text(
            json!({"v":1,"type":"session.authenticate","request_id":"auth","token":guest["token"]})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let mut first = Client {
        socket: first,
        guest,
        room: Value::Null,
        request: 0,
    };
    assert_eq!(
        first.response("auth").await["type"],
        "session.authenticated"
    );
    let second = server.socket("a").await;
    assert_eq!(server.runtime.metrics().active_connections, 2);
    assert_eq!(server.runtime.metrics().unauthenticated_connections, 1);
    assert!(
        connect_async(format!("ws://{}/v1/games/a/ws", server.address))
            .await
            .is_err(),
        "connection cap accepted third socket"
    );
    drop(first);
    drop(second);
    sleep(Duration::from_millis(100)).await;
    assert_eq!(server.runtime.metrics().active_connections, 0);
    assert_eq!(server.runtime.metrics().unauthenticated_connections, 0);
    let _replacement = server.socket("a").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn room_cap_refunds_rejected_and_disconnected_creation() {
    let config = Config {
        max_rooms: 1,
        reconnect_grace: Duration::from_millis(100),
        cleanup_interval: Duration::from_millis(10),
        ..Config::default()
    };
    let server = Server::new(config).await;
    let mut first = server.client("a").await;
    let mut other = server.client("a").await;
    assert_eq!(
        first
            .call("room.create", json!({"settings":{"capacity":0}}))
            .await["type"],
        "error"
    );
    assert_eq!(server.runtime.metrics().active_rooms, 0);
    assert_eq!(
        first
            .call("room.create", json!({"settings":{"speed":0}}))
            .await["type"],
        "error"
    );
    assert_eq!(server.runtime.metrics().active_rooms, 0);
    let room = first.enter(None).await;
    assert_error(
        &other.call("room.create", json!({"settings":{}})).await,
        "SERVER_BUSY",
    );
    assert_eq!(server.runtime.metrics().active_rooms, 1);
    assert_eq!(
        first.call("room.leave", room_ids(&room)).await["type"],
        "room.left"
    );
    sleep(Duration::from_millis(50)).await;
    assert_eq!(server.runtime.metrics().active_rooms, 0);
    other
        .send(
            json!({"v":1,"type":"room.create","request_id":"drop-during-admission","settings":{}}),
        )
        .await;
    drop(other);
    sleep(Duration::from_millis(300)).await;
    assert_eq!(
        server.runtime.metrics().active_rooms,
        0,
        "creation cancellation or reservation expiry leaked room cap"
    );
    first.enter(None).await;
}

async fn guest_request(server: &Server) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{}/v1/games/a/guests", server.address))
        .send()
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn issuer_and_global_guest_rate_limits_recover() {
    let issuer = Server::exact(Config {
        guest_burst: 2,
        guest_rate_per_second: 10,
        global_guest_burst: 100,
        ..Config::default()
    })
    .await;
    assert!(guest_request(&issuer).await.status().is_success());
    assert!(guest_request(&issuer).await.status().is_success());
    let limited = guest_request(&issuer).await;
    assert_eq!(limited.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        limited.json::<Value>().await.unwrap()["code"],
        "RATE_LIMITED"
    );
    sleep(Duration::from_millis(120)).await;
    assert!(guest_request(&issuer).await.status().is_success());
    let global = Server::exact(Config {
        guest_burst: 100,
        global_guest_burst: 2,
        global_guest_rate_per_second: 10,
        ..Config::default()
    })
    .await;
    assert!(guest_request(&global).await.status().is_success());
    assert!(guest_request(&global).await.status().is_success());
    let limited = guest_request(&global).await;
    assert_eq!(limited.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    let error = limited.json::<Value>().await.unwrap();
    assert_eq!(error["code"], "RATE_LIMITED");
    assert!(error["message"].as_str().unwrap().contains("global"));
    sleep(Duration::from_millis(120)).await;
    assert!(guest_request(&global).await.status().is_success());
}

async fn guest_with_trusted_source(server: &Server, octet: u8) -> reqwest::Response {
    // This models a trusted host adapter, never a user-controlled forwarded header.
    let source = SocketAddr::from(([127, 0, 0, octet], 10000));
    let app = server
        .runtime
        .router()
        .layer(axum::Extension(axum::extract::ConnectInfo(source)));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let response = reqwest::Client::new()
        .post(format!("http://{address}/v1/games/a/guests"))
        .send()
        .await
        .unwrap();
    task.abort();
    response
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rate_key_cap_and_ttl_reclaim_trusted_source_entries() {
    let server = Server::exact(Config {
        max_rate_keys: 1,
        guest_burst: 100,
        global_guest_burst: 100,
        rate_key_ttl: Duration::from_millis(150),
        cleanup_interval: Duration::from_millis(10),
        ..Config::default()
    })
    .await;
    assert!(
        guest_with_trusted_source(&server, 2)
            .await
            .status()
            .is_success()
    );
    assert_eq!(server.runtime.metrics().rate_keys, 1);
    let refused = guest_with_trusted_source(&server, 3).await;
    assert_eq!(refused.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        refused.json::<Value>().await.unwrap()["code"],
        "SERVER_BUSY"
    );
    assert_eq!(server.runtime.metrics().rate_keys, 1);
    sleep(Duration::from_millis(200)).await;
    assert_eq!(server.runtime.metrics().rate_keys, 0);
    assert!(
        guest_with_trusted_source(&server, 3)
            .await
            .status()
            .is_success()
    );
    assert_eq!(server.runtime.metrics().rate_keys, 1);
}

async fn response_or_closed(socket: &mut Socket, id: &str) -> Option<Value> {
    timeout(Duration::from_secs(3), async {
        loop {
            match socket.next().await {
                Some(Ok(Message::Text(text))) => {
                    let value: Value = serde_json::from_str(&text).unwrap();
                    if value["request_id"] == id {
                        return Some(value);
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return None,
                _ => {}
            }
        }
    })
    .await
    .expect("authentication/join neither responded nor closed")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_authentication_and_cross_room_admission_has_one_membership() {
    let server = Server::new(Config::default()).await;
    let mut owner_one = server.client("a").await;
    let first_room = owner_one.enter(None).await;
    let mut owner_two = server.client("a").await;
    let second_room = owner_two.enter(None).await;
    let guest = server.guest("a").await;
    let token = guest["token"].clone();
    let mut socket_one = server.socket("a").await;
    let mut socket_two = server.socket("a").await;
    let auth_one = Message::Text(
        json!({"v":1,"type":"session.authenticate","request_id":"auth-one","token":token})
            .to_string()
            .into(),
    );
    let auth_two = Message::Text(
        json!({"v":1,"type":"session.authenticate","request_id":"auth-two","token":token})
            .to_string()
            .into(),
    );
    let (sent_one, sent_two) = tokio::join!(socket_one.send(auth_one), socket_two.send(auth_two));
    sent_one.unwrap();
    sent_two.unwrap();
    let (authenticated_one, authenticated_two) = tokio::join!(
        response_or_closed(&mut socket_one, "auth-one"),
        response_or_closed(&mut socket_two, "auth-two")
    );
    assert!(
        [authenticated_one, authenticated_two]
            .iter()
            .flatten()
            .any(|value| value["type"] == "session.authenticated")
    );
    let join_one=Message::Text(json!({"v":1,"type":"room.join","request_id":"join-one","room_id":first_room["room_id"],"room_instance_id":first_room["room_instance_id"]}).to_string().into());
    let join_two=Message::Text(json!({"v":1,"type":"room.join","request_id":"join-two","room_id":second_room["room_id"],"room_instance_id":second_room["room_instance_id"]}).to_string().into());
    let (sent_one, sent_two) = tokio::join!(socket_one.send(join_one), socket_two.send(join_two));
    let receive = async {
        if sent_one.is_ok() {
            response_or_closed(&mut socket_one, "join-one").await
        } else {
            None
        }
    };
    let other = async {
        if sent_two.is_ok() {
            response_or_closed(&mut socket_two, "join-two").await
        } else {
            None
        }
    };
    let (joined_one, joined_two) = tokio::join!(receive, other);
    assert_eq!(
        [joined_one, joined_two]
            .iter()
            .flatten()
            .filter(|value| value["type"] == "room.entered")
            .count(),
        1
    );
    let listed = owner_one
        .call("room.list", json!({"cursor":null,"limit":20}))
        .await;
    assert_eq!(
        listed["rooms"]
            .as_array()
            .unwrap()
            .iter()
            .map(|room| room["members"].as_u64().unwrap())
            .sum::<u64>(),
        3,
        "shared guest gained two room memberships"
    );
    let mut current = Client {
        socket: server.socket("a").await,
        guest,
        room: Value::Null,
        request: 0,
    };
    let auth = current
        .call("session.authenticate", json!({"token":token}))
        .await;
    assert_eq!(auth["can_resume"], true);
    let room = room_ids(&auth);
    let resumed = current.call("resume", room.clone()).await;
    assert_eq!(resumed["type"], "room.resumed");
    current.room = resumed;
    drop(socket_one);
    drop(socket_two);
    current.input(1, 1.0, 0.0).await;
    current.snapshot_ack(1).await;
    let listed = owner_two
        .call("room.list", json!({"cursor":null,"limit":20}))
        .await;
    assert_eq!(
        listed["rooms"]
            .as_array()
            .unwrap()
            .iter()
            .map(|room| room["members"].as_u64().unwrap())
            .sum::<u64>(),
        3
    );
}
