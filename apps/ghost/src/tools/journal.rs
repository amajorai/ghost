use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::str_param;

const SHADOW_INGEST_ADDR: &str = "127.0.0.1:3030";

/// Resolve the shared-secret bearer Shadow's HTTP surface requires (everything
/// except `/health` — see `apps/shadow/src/server.rs`): `SHADOW_API_TOKEN` env
/// first (operator/Core-injected override), then the token file Core mints at
/// Shadow spawn (`~/.ryu/shadow/api-token` — the release-profile data dir,
/// matching the hardcoded release port above; `RYU_DIR` wins when exported),
/// then the standalone default `~/.shadow/api-token`. `None` = no token found;
/// Shadow rejects the ingest (fail closed) and the tool surfaces the error.
fn shadow_api_token() -> Option<String> {
    if let Ok(env_token) = std::env::var("SHADOW_API_TOKEN") {
        let trimmed = env_token.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(dir) = std::env::var("RYU_DIR") {
        let dir = dir.trim();
        if !dir.is_empty() {
            candidates.push(std::path::PathBuf::from(dir).join("shadow").join("api-token"));
        }
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".ryu").join("shadow").join("api-token"));
        candidates.push(home.join(".shadow").join("api-token"));
    }
    for path in candidates {
        if let Ok(existing) = std::fs::read_to_string(&path) {
            let trimmed = existing.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_owned());
            }
        }
    }
    None
}

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
    // Shadow's `/ingest` is bearer-gated like every non-health route.
    let auth_header = shadow_api_token()
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "POST /ingest HTTP/1.1\r\nHost: 127.0.0.1:3030\r\n{}Content-Type: application/json\r\nAccept: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        auth_header,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wall_micros_is_a_positive_unix_timestamp() {
        // Well past 2020 in microseconds — proves we read the wall clock, not 0.
        assert!(wall_micros() > 1_600_000_000_000_000);
    }

    // `SHADOW_API_TOKEN` is the highest-priority source and is read by no other test in
    // this crate. The two cases live in one test (not two) so they never race each other
    // on this single process-wide variable. Pins the security-relevant behaviour: an
    // explicit env token wins and is trimmed, and a blank one is rejected (fail-closed).
    #[test]
    fn shadow_api_token_env_resolution() {
        // SAFETY: edition 2021; the only writer of this var across the test binary.
        std::env::set_var("SHADOW_API_TOKEN", "  secret-token  ");
        let good = shadow_api_token();
        assert_eq!(good.as_deref(), Some("secret-token"), "env token wins, trimmed");

        // A whitespace-only env token must not be accepted as the bearer.
        std::env::set_var("SHADOW_API_TOKEN", "   ");
        let blank = shadow_api_token();
        std::env::remove_var("SHADOW_API_TOKEN");
        // It falls through past the blank env var; it must never be the blank string.
        assert_ne!(blank.as_deref(), Some(""));
        assert_ne!(blank.as_deref(), Some("   "));
    }
}
