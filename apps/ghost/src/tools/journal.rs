use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::str_param;

const SHADOW_INGEST_ADDR: &str = "127.0.0.1:3030";

pub async fn ghost_journal_marker(params: Value) -> Result<Value> {
    let title = str_param(&params, "title")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("title required"))?;
    let body = str_param(&params, "body").unwrap_or("").trim();
    let category = str_param(&params, "category").unwrap_or("Milestone").trim();
    let app = str_param(&params, "app").unwrap_or("Ghost").trim();

    let event = json!({
        "ts": wall_micros(),
        "v": 2,
        "track": 12,
        "type": "journal_marker",
        "app_name": if app.is_empty() { "Ghost" } else { app },
        "window_title": title,
        "source": "ghost",
        "category": if category.is_empty() { "Milestone" } else { category },
        "body": body,
    });
    let body_json = json!({ "events": [event] }).to_string();
    let response = tokio::task::spawn_blocking(move || post_to_shadow(&body_json)).await??;

    Ok(json!({
        "ok": true,
        "shadow_response": response,
        "suggestion": "The marker is now available in Shadow's journal and desktop timeline."
    }))
}

fn post_to_shadow(body: &str) -> Result<String> {
    let mut stream = TcpStream::connect(SHADOW_INGEST_ADDR)
        .context("Shadow is not reachable at 127.0.0.1:3030")?;
    let request = format!(
        "POST /ingest HTTP/1.1\r\nHost: 127.0.0.1:3030\r\nContent-Type: application/json\r\nAccept: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(request.as_bytes())?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let payload = response
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or(response.as_str())
        .trim()
        .to_string();
    Ok(payload)
}

fn wall_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}
