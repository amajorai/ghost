use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use ryu_sidecar_runtime::peer_proof;
use serde_json::{json, Value};

use super::str_param;

/// Shadow's canonical (release-profile) port; other profiles shift it.
const SHADOW_BASE_PORT: u16 = 3030;
/// Bound on connect/read/write so a wedged Shadow cannot hang the tool call.
const SHADOW_IO_TIMEOUT: Duration = Duration::from_secs(5);
/// Largest `/health` response (headers and body) the peer probe reads. A real
/// one is a few hundred bytes; a listener streaming an endless reply is cut off.
const MAX_HEALTH_RESPONSE_BYTES: u64 = 16 * 1024;

/// Shadow's `host:port`: `RYU_SHADOW_URL` (Core's per-profile export) when it is a
/// plain `http://` URL, else the profile-shifted loopback port (`RYU_PROFILE` /
/// `RYU_PORT_OFFSET`), matching where Core starts Shadow.
fn shadow_ingest_addr() -> String {
    std::env::var("RYU_SHADOW_URL")
        .ok()
        .and_then(|url| addr_from_http_url(&url))
        .unwrap_or_else(|| {
            format!(
                "127.0.0.1:{}",
                ryu_sidecar_runtime::profiled_port(SHADOW_BASE_PORT)
            )
        })
}

/// `host:port` from an `http://host[:port][/path]` URL. `None` for anything this
/// plain-TCP client cannot dial (https, userinfo, empty host).
///
/// A `localhost` name is pinned to `127.0.0.1`, as `peer_proof::pin_localhost`
/// does for Core: the probe and the bearer request use separate connections,
/// and a name that resolves to `::1` first would let any process listening
/// there relay the challenge to the real Shadow and then receive the bearer.
fn addr_from_http_url(url: &str) -> Option<String> {
    let rest = url.trim().strip_prefix("http://")?;
    let authority = rest.split(['/', '?', '#']).next()?.trim();
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let (host, port) = match authority.rfind(']') {
        Some(bracket) => match authority[bracket..].find(':') {
            Some(colon) => authority.split_at(bracket + colon),
            None => (authority, ""),
        },
        None => match authority.rfind(':') {
            Some(colon) => authority.split_at(colon),
            None => (authority, ""),
        },
    };
    let name = host.trim_end_matches('.').to_ascii_lowercase();
    let host = if name == "localhost" || name.ends_with(".localhost") {
        "127.0.0.1"
    } else {
        host
    };
    Some(if port.is_empty() {
        format!("{host}:80")
    } else {
        format!("{host}{port}")
    })
}

/// Resolve the shared-secret bearer Shadow's HTTP surface requires (everything
/// except `/health` — see `apps/shadow/src/server.rs`): `SHADOW_API_TOKEN` env
/// first (operator/Core-injected override), then the token file Core mints at
/// Shadow spawn: `$SHADOW_DATA_DIR/api-token`, else the Ryu data dir's
/// `services/shadow/api-token` (resolved like every Ryu process: `RYU_DIR`, the
/// Storage pointer, then the profile default). `None` = no token found; the tool
/// fails before sending, since Shadow would reject the ingest (fail closed).
fn shadow_api_token() -> Option<String> {
    if let Ok(env_token) = std::env::var("SHADOW_API_TOKEN") {
        let trimmed = env_token.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Some(dir) = std::env::var_os("SHADOW_DATA_DIR").filter(|dir| !dir.is_empty()) {
        candidates.push(std::path::PathBuf::from(dir).join("api-token"));
    }
    candidates.push(
        ryu_sidecar_runtime::layout::service_dir(&ryu_sidecar_runtime::ryu_dir(), "shadow")
            .join("api-token"),
    );
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

    Ok(marker_outcome(&response))
}

/// Interpret Shadow's `/ingest` 2xx body. Shadow answers 200 with
/// `{"ingested":0,"paused":true}` while capture is paused, and `ingested: 0` when the
/// event could not be written; neither recorded the marker.
fn marker_outcome(response: &str) -> Value {
    let parsed: Value = serde_json::from_str(response).unwrap_or(Value::Null);
    let shadow_response = if parsed.is_null() {
        Value::String(response.to_owned())
    } else {
        parsed.clone()
    };
    if parsed["paused"].as_bool() == Some(true) {
        return json!({
            "ok": false,
            "recorded": false,
            "shadow_response": shadow_response,
            "reason": "Shadow capture is paused, so the marker was not recorded. Resume capture and retry."
        });
    }
    if parsed["ingested"].as_u64().unwrap_or(0) < 1 {
        return json!({
            "ok": false,
            "recorded": false,
            "shadow_response": shadow_response,
            "reason": "Shadow accepted the request but did not record the marker."
        });
    }
    json!({
        "ok": true,
        "recorded": true,
        "shadow_response": shadow_response,
        "suggestion": "The marker is now available in Shadow's journal and desktop timeline."
    })
}

fn post_to_shadow(body: &str) -> Result<String> {
    // Shadow's `/ingest` is bearer-gated like every non-health route.
    let token = shadow_api_token().ok_or_else(|| {
        anyhow::anyhow!(
            "No Shadow API token found (SHADOW_API_TOKEN, $SHADOW_DATA_DIR/api-token, or the \
             Ryu data dir's services/shadow/api-token); Shadow would reject the marker"
        )
    })?;
    post_to_shadow_at(&shadow_ingest_addr(), &token, body)
}

/// Post `body` to `/ingest` at `addr` with the bearer, but only after the
/// listener there proved it holds `token` ([`verify_shadow_peer`]). Any process
/// can bind Shadow's port while Shadow is down, so an unproven listener gets
/// neither the token nor the marker.
fn post_to_shadow_at(addr: &str, token: &str, body: &str) -> Result<String> {
    verify_shadow_peer(addr, token)?;
    let mut stream = connect_shadow(addr)?;
    stream.set_read_timeout(Some(SHADOW_IO_TIMEOUT))?;
    stream.set_write_timeout(Some(SHADOW_IO_TIMEOUT))?;
    let request = format!(
        "POST /ingest HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nAccept: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(request.as_bytes())?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .with_context(|| format!("reading Shadow's response from {addr}"))?;
    let (status, payload) = parse_http_response(&response)?;
    if status == 401 || status == 403 {
        anyhow::bail!(
            "Shadow rejected the API token (HTTP {status}); the token does not match this Shadow at {addr}"
        );
    }
    if !(200..300).contains(&status) {
        anyhow::bail!("Shadow /ingest at {addr} returned HTTP {status}: {payload}");
    }
    Ok(payload.to_owned())
}

/// Run the loopback peer proof (`ryu_sidecar_runtime::peer_proof`) against the
/// listener at `addr`: send a fresh challenge to the unauthenticated
/// `/health?challenge=...` and require Shadow's HMAC proof that it holds
/// `token`. The token itself is never sent here. Fails closed: a listener that
/// gives no proof, a wrong one, or no valid health answer is not Shadow.
fn verify_shadow_peer(addr: &str, token: &str) -> Result<()> {
    let challenge = peer_proof::new_challenge();
    let mut stream = connect_shadow(addr)?;
    stream.set_read_timeout(Some(SHADOW_IO_TIMEOUT))?;
    stream.set_write_timeout(Some(SHADOW_IO_TIMEOUT))?;
    let request = format!(
        "GET /health?{}={challenge} HTTP/1.1\r\nHost: {addr}\r\nAccept: application/json\r\nConnection: close\r\n\r\n",
        peer_proof::CHALLENGE_QUERY_PARAM
    );
    stream.write_all(request.as_bytes())?;
    let mut raw = Vec::new();
    stream
        .take(MAX_HEALTH_RESPONSE_BYTES)
        .read_to_end(&mut raw)
        .with_context(|| format!("reading Shadow's health response from {addr}"))?;
    let response = String::from_utf8_lossy(&raw);
    let proven = parse_http_response(&response)
        .is_ok_and(|(status, payload)| health_proves_token(token, &challenge, status, payload));
    if !proven {
        anyhow::bail!(
            "The listener at {addr} did not prove it is Shadow (no valid peer proof on /health); \
             the API token and the marker were not sent. Restart Shadow from Ryu, or free the port \
             if another process holds it."
        );
    }
    Ok(())
}

/// Whether a `/health` reply (`status`, JSON `payload`) proves its sender holds
/// `token` for `challenge`: a 2xx status and a `proof` that verifies in
/// constant time.
fn health_proves_token(token: &str, challenge: &str, status: u16, payload: &str) -> bool {
    if !(200..300).contains(&status) {
        return false;
    }
    serde_json::from_str::<Value>(payload)
        .ok()
        .and_then(|health| {
            health
                .get(peer_proof::PROOF_FIELD)?
                .as_str()
                .map(str::to_owned)
        })
        .is_some_and(|provided| peer_proof::verify(token, challenge, &provided))
}

/// Dial every address `addr` resolves to until one accepts. Shadow binds only
/// 127.0.0.1, so a hostname such as `localhost` that resolves to `::1` first must
/// still fall through to the IPv4 address.
fn connect_shadow(addr: &str) -> Result<TcpStream> {
    let mut last_err: Option<std::io::Error> = None;
    for socket_addr in addr
        .to_socket_addrs()
        .with_context(|| format!("invalid Shadow address {addr}"))?
    {
        match TcpStream::connect_timeout(&socket_addr, SHADOW_IO_TIMEOUT) {
            Ok(stream) => return Ok(stream),
            Err(err) => last_err = Some(err),
        }
    }
    match last_err {
        Some(err) => Err(err).with_context(|| format!("Shadow is not reachable at {addr}")),
        None => anyhow::bail!("Shadow address {addr} did not resolve"),
    }
}

/// Split a `Connection: close` HTTP/1.1 response into its status code and trimmed body.
fn parse_http_response(response: &str) -> Result<(u16, &str)> {
    let status = response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| anyhow::anyhow!("Shadow returned a malformed HTTP response"))?;
    let body = response
        .split_once("\r\n\r\n")
        .map_or("", |(_, body)| body)
        .trim();
    Ok((status, body))
}

fn wall_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

#[cfg(test)]
mod tests;
