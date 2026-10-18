//! Direct protocol walkthrough; no game SDK is required.
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{error::Error, time::Duration};
use tokio::{net::TcpStream, time::timeout};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type Result<T> = std::result::Result<T, Box<dyn Error>>;

async fn receive(socket: &mut Socket) -> Result<Value> {
    loop {
        match timeout(Duration::from_secs(5), socket.next())
            .await?
            .ok_or("Socket closed")??
        {
            Message::Text(text) => return Ok(serde_json::from_str(&text)?),
            Message::Close(_) => return Err("Socket closed".into()),
            _ => {}
        }
    }
}

async fn request(socket: &mut Socket, body: Value) -> Result<Value> {
    socket.send(Message::Text(body.to_string().into())).await?;
    loop {
        let response = receive(socket).await?;
        if response["request_id"] == body["request_id"] {
            if response["type"] == "error" {
                return Err(format!("{}: {}", response["code"], response["message"]).into());
            }
            return Ok(response);
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let base = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "http://127.0.0.1:3000".into());
    let game = std::env::args().nth(2).unwrap_or_else(|| "arena".into());
    let client = reqwest::Client::new();
    let guest: Value = client
        .post(format!("{base}/v1/games/{game}/guests"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let token = guest["token"].as_str().ok_or("Missing guest token")?;
    let ws_base = base
        .replacen("http://", "ws://", 1)
        .replacen("https://", "wss://", 1);
    let endpoint = format!("{ws_base}/v1/games/{game}/ws");
    let (mut socket, _) = connect_async(&endpoint).await?;
    let auth = request(
        &mut socket,
        json!({"v":1,"type":"session.authenticate","request_id":"auth-1","token":token}),
    )
    .await?;
    assert_eq!(auth["guest_id"], guest["guest_id"]);
    let entered = request(&mut socket, json!({"v":1,"type":"room.create","request_id":"create-1","settings":{"capacity":16,"public":true}})).await?;
    let room = entered["room_id"]
        .as_str()
        .ok_or("Missing room ID")?
        .to_owned();
    let instance = entered["room_instance_id"]
        .as_str()
        .ok_or("Missing room instance")?
        .to_owned();
    request(&mut socket, json!({"v":1,"type":"game.input","request_id":"input-1","room_id":room,"room_instance_id":instance,"input_seq":1,"payload":{"move_x":1,"move_y":0}})).await?;
    let processed = loop {
        let snapshot = receive(&mut socket).await?;
        if snapshot["type"] == "snapshot"
            && snapshot["last_processed_input_seq"].as_u64().unwrap_or(0) >= 1
        {
            break snapshot;
        }
    };
    // A new authentication explicitly fences the old connection before resume.
    socket.close(None).await?;
    drop(socket);
    let (mut socket, _) = connect_async(&endpoint).await?;
    let auth = request(
        &mut socket,
        json!({"v":1,"type":"session.authenticate","request_id":"auth-2","token":token}),
    )
    .await?;
    assert_eq!(auth["can_resume"], true);
    let resumed = request(&mut socket, json!({"v":1,"type":"resume","request_id":"resume-1","room_id":room,"room_instance_id":instance})).await?;
    assert_eq!(resumed["room_instance_id"], instance);
    assert_eq!(
        resumed["last_processed_input_seq"],
        processed["last_processed_input_seq"]
    );
    assert!(resumed["server_tick"].as_u64().unwrap() >= processed["server_tick"].as_u64().unwrap());
    let next = resumed["last_processed_input_seq"].as_u64().unwrap() + 1;
    request(&mut socket, json!({"v":1,"type":"game.input","request_id":"input-2","room_id":room,"room_instance_id":instance,"input_seq":next,"payload":{"move_x":0,"move_y":0}})).await?;
    request(&mut socket, json!({"v":1,"type":"room.leave","request_id":"leave-1","room_id":room,"room_instance_id":instance})).await?;
    request(
        &mut socket,
        json!({"v":1,"type":"session.logout","request_id":"logout-1"}),
    )
    .await?;
    println!(
        "guest → authenticate → create → input/snapshot → reconnect → resume → leave → logout: OK"
    );
    Ok(())
}
