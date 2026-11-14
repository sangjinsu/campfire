//! Load generator for a separately running server, so server CPU can be profiled on its own.
//!
//! ```sh
//! cargo run --release -p campfire-server --example load -- \
//!     http://127.0.0.1:3000 arena --rooms 64 --members 16 --seconds 30 --input-hz 20
//! ```
//!
//! `errors` counts closed connections and error replies; a clean run reports 0.
//! Keep `--input-hz` at or below the server's `--input-rate-per-second` (60 by default).
//!
//! The server needs guest limits that admit every client, for example
//! `--guest-burst 100000 --global-guest-burst 100000`.

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::watch;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Default)]
struct Totals {
    inputs_sent: AtomicU64,
    inputs_accepted: AtomicU64,
    snapshots: AtomicU64,
    snapshot_bytes: AtomicU64,
    errors: AtomicU64,
}

async fn call(socket: &mut Socket, request: Value, expected: &str) -> Result<Value, String> {
    socket
        .send(Message::Text(request.to_string().into()))
        .await
        .map_err(|error| error.to_string())?;
    loop {
        let message = socket
            .next()
            .await
            .ok_or("connection closed")?
            .map_err(|error| error.to_string())?;
        let Message::Text(text) = message else {
            continue;
        };
        let value: Value = serde_json::from_str(&text).map_err(|error| error.to_string())?;
        if value["type"] == expected {
            return Ok(value);
        }
        if value["type"] == "error" {
            return Err(value.to_string());
        }
    }
}

async fn connect(http: &reqwest::Client, base: &str, game: &str) -> Result<Socket, String> {
    let issued: Value = http
        .post(format!("{base}/v1/games/{game}/guests"))
        .send()
        .await
        .map_err(|error| error.to_string())?
        .json()
        .await
        .map_err(|error| error.to_string())?;
    let token = issued["token"]
        .as_str()
        .ok_or_else(|| format!("guest issue failed: {issued}"))?;
    let url = format!("{}/v1/games/{game}/ws", base.replacen("http", "ws", 1));
    let (mut socket, _) = connect_async(url)
        .await
        .map_err(|error| error.to_string())?;
    call(
        &mut socket,
        json!({"v":1,"type":"session.authenticate","request_id":"auth","token":token}),
        "session.authenticated",
    )
    .await?;
    Ok(socket)
}

async fn play(
    mut socket: Socket,
    room: Value,
    index: usize,
    period: Duration,
    duration: Duration,
    mut start: watch::Receiver<bool>,
    totals: Arc<Totals>,
) {
    // Keep reading while the other clients connect. The server sends snapshots from
    // the moment a client enters, and an unread socket would eventually be closed
    // by the server's write deadline.
    loop {
        tokio::select! {
            changed = start.changed() => {
                if changed.is_err() { return; }
                break;
            }
            message = socket.next() => {
                if !matches!(message, Some(Ok(_))) {
                    totals.errors.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            }
        }
    }
    let deadline = Instant::now() + duration;
    let mut ticker = tokio::time::interval(period);
    // After a stall, skip missed sends instead of bursting past the server's input budget.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut sequence = 0_u64;
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline.into()) => break,
            _ = ticker.tick() => {
                sequence += 1;
                let direction = if (sequence / 15).is_multiple_of(2) { 1.0 } else { -1.0 };
                let (x, y) = if index.is_multiple_of(2) { (direction, 0.0) } else { (0.0, direction) };
                let request = json!({"v":1,"type":"game.input","request_id":format!("i{sequence}"),
                    "room_id":room["room_id"],"room_instance_id":room["room_instance_id"],
                    "input_seq":sequence,"payload":{"move_x":x,"move_y":y}});
                if socket.send(Message::Text(request.to_string().into())).await.is_err() {
                    totals.errors.fetch_add(1, Ordering::Relaxed);
                    return;
                }
                totals.inputs_sent.fetch_add(1, Ordering::Relaxed);
            }
            message = socket.next() => {
                let Some(Ok(message)) = message else {
                    totals.errors.fetch_add(1, Ordering::Relaxed);
                    return;
                };
                let Message::Text(text) = message else { continue };
                // Classify without a full parse so the generator stays cheaper than the server.
                if text.contains(r#""type":"snapshot""#) {
                    totals.snapshots.fetch_add(1, Ordering::Relaxed);
                    totals.snapshot_bytes.fetch_add(text.len() as u64, Ordering::Relaxed);
                } else if text.contains("game.input.accepted") {
                    totals.inputs_accepted.fetch_add(1, Ordering::Relaxed);
                } else if text.contains(r#""type":"error""#) {
                    totals.errors.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
}

fn option(args: &[String], name: &str, default: u64) -> u64 {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|index| args.get(index + 1))
        .map_or(default, |value| {
            value
                .parse()
                .unwrap_or_else(|_| panic!("{name} expects a number"))
        })
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(base), Some(game)) = (args.first().cloned(), args.get(1).cloned()) else {
        eprintln!(
            "usage: load <http://host:port> <game> [--rooms N] [--members N] [--seconds N] [--input-hz N]"
        );
        std::process::exit(2);
    };
    let rooms = option(&args, "--rooms", 4) as usize;
    let members = option(&args, "--members", 16) as usize;
    let seconds = option(&args, "--seconds", 30);
    let input_hz = option(&args, "--input-hz", 20).max(1);

    let http = reqwest::Client::new();
    let totals = Arc::new(Totals::default());
    let (start, started) = watch::channel(false);
    let period = Duration::from_secs_f64(1.0 / input_hz as f64);
    let duration = Duration::from_secs(seconds);
    let mut tasks = Vec::new();
    for _ in 0..rooms {
        let mut room = None;
        for _ in 0..members {
            let mut socket = connect(&http, &base, &game).await.expect("client connects");
            let request = match &room {
                None => {
                    json!({"v":1,"type":"room.create","request_id":"enter","settings":{"capacity":members}})
                }
                Some(room) => {
                    let room: &Value = room;
                    json!({"v":1,"type":"room.join","request_id":"enter","room_id":room["room_id"],
                        "room_instance_id":room["room_instance_id"]})
                }
            };
            let entered = call(&mut socket, request, "room.entered")
                .await
                .expect("client enters the room");
            let room = room.get_or_insert(entered).clone();
            tasks.push(tokio::spawn(play(
                socket,
                room,
                tasks.len(),
                period,
                duration,
                started.clone(),
                totals.clone(),
            )));
        }
    }
    eprintln!("connected {} clients in {rooms} rooms", tasks.len());
    let began = Instant::now();
    start.send_replace(true);
    for task in tasks {
        let _ = task.await;
    }
    let elapsed = began.elapsed().as_secs_f64();
    let load = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
    println!(
        "{}",
        json!({
            "rooms": rooms, "members": members, "clients": rooms * members, "input_hz": input_hz,
            "elapsed_seconds": elapsed,
            "inputs_sent_per_second": load(&totals.inputs_sent) as f64 / elapsed,
            "inputs_accepted_per_second": load(&totals.inputs_accepted) as f64 / elapsed,
            "snapshots_per_second": load(&totals.snapshots) as f64 / elapsed,
            "snapshot_megabytes_per_second": load(&totals.snapshot_bytes) as f64 / elapsed / 1e6,
            "errors": load(&totals.errors),
        })
    );
}
