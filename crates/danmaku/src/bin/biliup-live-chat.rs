use std::fs::OpenOptions;
use std::io::Write;
use std::time::Duration;

use danmaku::client::connect_websocket;
use danmaku::protocols::bilibili::Bilibili;
use danmaku::protocols::{HeartbeatData, RegistrationData};
use danmaku::{DanmakuEvent, Platform, PlatformContext};
use futures::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let room = args.next().ok_or("Missing Bilibili room ID")?;
    let path = args.next().ok_or("Missing feed file")?;
    if room.is_empty() || !room.bytes().all(|byte| byte.is_ascii_digit()) || args.next().is_some() {
        return Err("Expected a Bilibili room ID and a feed file".into());
    }
    let mut output = OpenOptions::new().append(true).open(path)?;
    let platform = Bilibili::new();
    let url = format!("https://live.bilibili.com/{room}");
    let connection = platform
        .get_connection_info(&url, &PlatformContext::new())
        .await?;
    let mut socket = tokio::time::timeout(
        Duration::from_secs(12),
        connect_websocket(&connection, platform.name()),
    )
    .await??;
    for registration in connection.registration_data {
        let message = match registration {
            RegistrationData::Binary(bytes) => Message::Binary(bytes.into()),
            RegistrationData::Text(text) => Message::Text(text.into()),
        };
        socket.send(message).await?;
    }
    let heartbeat = platform.heartbeat_config();
    let Some(HeartbeatData::Binary(payload)) = heartbeat.data else {
        return Err("Missing Bilibili heartbeat".into());
    };
    let mut heartbeat_tick = tokio::time::interval(heartbeat.interval);
    let lifetime = tokio::time::sleep(Duration::from_secs(120));
    tokio::pin!(lifetime);

    loop {
        tokio::select! {
            _ = &mut lifetime => return Ok(()),
            _ = heartbeat_tick.tick() => {
                socket.send(Message::Binary(payload.clone().into())).await?;
            }
            frame = socket.next() => match frame.transpose()? {
                Some(Message::Binary(bytes)) => {
                    if let Ok(decoded) = platform.decode_message(&bytes) {
                        for event in decoded.events {
                            if let DanmakuEvent::Chat(chat) = event {
                                let row = json!({"kind": "danmaku", "text": chat.content, "color": chat.color});
                                writeln!(output, "data: {row}")?;
                            }
                        }
                    }
                }
                Some(Message::Close(_)) | None => return Err("Bilibili closed the chat connection".into()),
                Some(_) => {}
            }
        }
    }
}
