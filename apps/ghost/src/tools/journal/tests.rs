use super::*;
use std::net::TcpListener;
use std::sync::{Mutex, PoisonError};

/// Serialises the tests that write the process-wide env vars this module reads
/// (`SHADOW_API_TOKEN`, `RYU_PORT_OFFSET`).
static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn addr_from_http_url_extracts_host_and_port() {
    assert_eq!(
        addr_from_http_url("http://127.0.0.1:4030").as_deref(),
        Some("127.0.0.1:4030")
    );
    assert_eq!(
        addr_from_http_url(" http://localhost:4030/ingest ").as_deref(),
        Some("127.0.0.1:4030")
    );
    assert_eq!(
        addr_from_http_url("http://Shadow.LOCALHOST.:4030").as_deref(),
        Some("127.0.0.1:4030")
    );
    assert_eq!(
        addr_from_http_url("http://[::1]:4030").as_deref(),
        Some("[::1]:4030")
    );
    assert_eq!(
        addr_from_http_url("http://localhost").as_deref(),
        Some("127.0.0.1:80")
    );
    assert_eq!(
        addr_from_http_url("http://notlocalhost:4030").as_deref(),
        Some("notlocalhost:4030")
    );
    assert_eq!(addr_from_http_url("https://127.0.0.1:4030"), None);
    assert_eq!(addr_from_http_url("http://user@127.0.0.1:4030"), None);
    assert_eq!(addr_from_http_url("http://"), None);
}

// `localhost` may resolve to ::1 before 127.0.0.1; Shadow binds only IPv4, so the
// dialer must try every resolved address rather than the first one alone.
#[test]
fn connect_shadow_tries_every_resolved_address() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().expect("local addr").port();
    connect_shadow(&format!("localhost:{port}")).expect("falls through to 127.0.0.1");
    drop(listener);
    let err = connect_shadow(&format!("127.0.0.1:{port}")).expect_err("nothing listening");
    assert!(err.to_string().contains("Shadow is not reachable"), "{err}");
}

#[test]
fn parse_http_response_reads_status_and_body() {
    let (status, body) =
        parse_http_response("HTTP/1.1 401 Unauthorized\r\ncontent-length: 12\r\n\r\nunauthorized")
            .unwrap();
    assert_eq!(status, 401);
    assert_eq!(body, "unauthorized");
    assert!(parse_http_response("garbage").is_err());
}

/// How a fake listener answers `/health?challenge=...`.
#[derive(Clone, Copy)]
enum FakeHealth {
    /// Real Shadow: the proof for its token.
    Proves(&'static str),
    /// A squatter, or a Shadow that predates the proof: no `proof` field.
    NoProof,
}

/// A one-thread loopback listener that answers `/health` per `health` and
/// `/ingest` with `{"ingested":1}`, and records every raw request it read.
fn fake_shadow(
    health: FakeHealth,
) -> (
    String,
    std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    std::thread::JoinHandle<()>,
) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr").to_string();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = std::sync::Arc::clone(&seen);
    let handle = std::thread::spawn(move || {
        // Up to two requests: the health probe and (only when proven) the ingest.
        for _ in 0..2 {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("read timeout");
            let mut buf = [0_u8; 8192];
            let read = stream.read(&mut buf).unwrap_or(0);
            if read == 0 {
                return;
            }
            let request = String::from_utf8_lossy(&buf[..read]).into_owned();
            recorded.lock().unwrap().push(request.clone());
            let body = if request.starts_with("GET /health?") {
                let challenge = request
                    .split_whitespace()
                    .nth(1)
                    .and_then(|target| target.split_once("challenge="))
                    .map(|(_, challenge)| challenge.to_owned())
                    .unwrap_or_default();
                let proof = match health {
                    FakeHealth::Proves(token) => peer_proof::proof(token, &challenge),
                    FakeHealth::NoProof => None,
                };
                json!({ "status": "healthy", "proof": proof }).to_string()
            } else {
                json!({ "ingested": 1 }).to_string()
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            if matches!(health, FakeHealth::NoProof) {
                return;
            }
        }
    });
    (addr, seen, handle)
}

#[test]
fn a_proven_shadow_receives_the_bearer_and_the_marker() {
    let (addr, seen, handle) = fake_shadow(FakeHealth::Proves("the-token"));
    let response =
        post_to_shadow_at(&addr, "the-token", r#"{"events":[]}"#).expect("posted to Shadow");
    handle.join().expect("fake Shadow thread");
    assert_eq!(marker_outcome(&response)["recorded"], true);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2, "probe then ingest: {seen:?}");
    assert!(
        !seen[0].contains("Authorization"),
        "the probe never carries the token"
    );
    assert!(!seen[0].contains("the-token"));
    assert!(seen[1].starts_with("POST /ingest "));
    assert!(seen[1].contains("Authorization: Bearer the-token"));
}

#[test]
fn an_unproven_listener_never_receives_the_token_or_the_marker() {
    for health in [FakeHealth::NoProof, FakeHealth::Proves("another-token")] {
        let (addr, seen, handle) = fake_shadow(health);
        let err = post_to_shadow_at(&addr, "the-token", r#"{"events":["private"]}"#)
            .expect_err("an unproven listener is not Shadow");
        assert!(
            err.to_string().contains("did not prove it is Shadow"),
            "{err}"
        );
        // Unblock a fake still waiting for a second connection, then join.
        let _ = TcpStream::connect(&addr);
        handle.join().expect("fake listener thread");
        let seen = seen.lock().unwrap();
        assert!(
            seen.iter()
                .all(|request| request.starts_with("GET /health?")
                    && !request.contains("the-token")
                    && !request.contains("private")),
            "only the token-free probe reached the listener: {seen:?}"
        );
    }
}

#[test]
fn health_proves_token_requires_a_2xx_and_a_matching_proof() {
    let challenge = peer_proof::new_challenge();
    let good = peer_proof::proof("the-token", &challenge).unwrap();
    let body = json!({ "status": "healthy", "proof": good }).to_string();
    assert!(health_proves_token("the-token", &challenge, 200, &body));
    assert!(!health_proves_token("the-token", &challenge, 503, &body));
    assert!(!health_proves_token("rotated", &challenge, 200, &body));
    let other = peer_proof::new_challenge();
    assert!(!health_proves_token("the-token", &other, 200, &body));
    assert!(!health_proves_token(
        "the-token",
        &challenge,
        200,
        r#"{"status":"healthy"}"#
    ));
    assert!(!health_proves_token(
        "the-token",
        &challenge,
        200,
        "not json"
    ));
}

#[test]
fn marker_outcome_reports_paused_and_unwritten_as_not_recorded() {
    let paused = marker_outcome(r#"{"ingested":0,"paused":true}"#);
    assert_eq!(paused["ok"], false);
    assert!(paused["reason"].as_str().unwrap().contains("paused"));
    assert_eq!(marker_outcome(r#"{"ingested":0}"#)["ok"], false);
    assert_eq!(marker_outcome("not json")["ok"], false);
    let recorded = marker_outcome(r#"{"ingested":1}"#);
    assert_eq!(recorded["ok"], true);
    assert_eq!(recorded["recorded"], true);
}

#[test]
fn wall_micros_is_a_positive_unix_timestamp() {
    // Well past 2020 in microseconds — proves we read the wall clock, not 0.
    assert!(wall_micros() > 1_600_000_000_000_000);
}

// `SHADOW_API_TOKEN` is the highest-priority source. The two cases live in one test
// (not two) so they never race each other on this single process-wide variable, and
// `ENV_LOCK` keeps the delivery test below from writing it concurrently. Pins the
// security-relevant behaviour: an explicit env token wins and is trimmed, and a
// blank one is rejected (fail-closed).
#[test]
fn shadow_api_token_env_resolution() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    // SAFETY: edition 2021; env writers in this test binary hold `ENV_LOCK`.
    std::env::set_var("SHADOW_API_TOKEN", "  secret-token  ");
    let good = shadow_api_token();
    assert_eq!(
        good.as_deref(),
        Some("secret-token"),
        "env token wins, trimmed"
    );

    // A whitespace-only env token must not be accepted as the bearer.
    std::env::set_var("SHADOW_API_TOKEN", "   ");
    let blank = shadow_api_token();
    std::env::remove_var("SHADOW_API_TOKEN");
    // It falls through past the blank env var; it must never be the blank string.
    assert_ne!(blank.as_deref(), Some(""));
    assert_ne!(blank.as_deref(), Some("   "));
}

/// Restores an env var to its value from before the test on drop, so a failing
/// assertion cannot leak the override into later tests.
struct EnvRestore {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvRestore {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

/// Bind a loopback listener on a port above Shadow's base port that a valid
/// `RYU_PORT_OFFSET` (at most 50 000) can reach, returning it with that offset.
///
/// Binds explicit ports rather than asking the OS for an ephemeral one: the
/// ephemeral range is 49152-65535 on Windows, mostly beyond the 50 000 offset
/// cap, so `127.0.0.1:0` would rarely land inside it there. The start point is
/// spread by process id so parallel runs do not race for the same port.
fn bind_profile_shifted_shadow_port() -> (TcpListener, u16) {
    const MAX_OFFSET: u32 = 50_000;
    // Offsets below 1 000 are the dev profile's own shift; stay clear of them.
    const FIRST_OFFSET: u32 = 1_000;
    let span = MAX_OFFSET - FIRST_OFFSET + 1;
    let start = std::process::id() % span;
    for step in 0..span {
        let offset = u16::try_from(FIRST_OFFSET + (start + step) % span)
            .expect("offsets are at most 50 000");
        if let Ok(listener) = TcpListener::bind(("127.0.0.1", SHADOW_BASE_PORT + offset)) {
            return (listener, offset);
        }
    }
    panic!("no free loopback port within the profile offset range");
}

/// A stand-in for Shadow: answers the peer-proof `/health` probe with a proof
/// for `token`, then accepts the ingest connection, captures its raw request
/// (head plus `Content-Length` body), answers `200 {"ok":true}` and closes.
/// Returns the ingest request.
fn serve_one_request(
    listener: TcpListener,
    token: &'static str,
) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || loop {
        let (mut conn, _) = listener.accept().expect("accept a connection");
        let mut raw = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let read = conn.read(&mut chunk).expect("read the request");
            if read == 0 {
                break;
            }
            raw.extend_from_slice(&chunk[..read]);
            let text = String::from_utf8_lossy(&raw);
            if let Some(head_end) = text.find("\r\n\r\n") {
                let content_length = text[..head_end]
                    .lines()
                    .find_map(|line| line.strip_prefix("Content-Length: "))
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if raw.len() >= head_end + 4 + content_length {
                    break;
                }
            }
        }
        let request = String::from_utf8(raw).expect("utf-8 request");
        if request.starts_with("GET /health?") {
            let challenge = request
                .split_whitespace()
                .nth(1)
                .and_then(|target| target.split_once("challenge="))
                .map(|(_, challenge)| challenge.to_owned())
                .unwrap_or_default();
            let body = json!({
                "status": "healthy",
                "proof": peer_proof::proof(token, &challenge),
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            conn.write_all(response.as_bytes())
                .expect("write the health response");
            continue;
        }
        conn.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"ok\":true}",
        )
        .expect("write the ingest response");
        return request;
    })
}

#[test]
fn shadow_ingest_addr_follows_the_profile_port_offset() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    {
        let _offset = EnvRestore::set("RYU_PORT_OFFSET", "0");
        assert_eq!(
            shadow_ingest_addr(),
            "127.0.0.1:3030",
            "release is unchanged"
        );
    }
    let _offset = EnvRestore::set("RYU_PORT_OFFSET", "1000");
    assert_eq!(shadow_ingest_addr(), "127.0.0.1:4030", "dev-shifted Shadow");
}

// The bearer must reach the Shadow the active profile actually runs, never a fixed
// `127.0.0.1:3030` that any local process can bind while Shadow listens on the
// profile-shifted port (dev 4030, canary 5030, …).
#[test]
fn post_to_shadow_sends_the_bearer_to_the_profile_shifted_shadow_port() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _token = EnvRestore::set("SHADOW_API_TOKEN", "ghost-test-token");
    let (listener, offset) = bind_profile_shifted_shadow_port();
    let shifted_port = SHADOW_BASE_PORT + offset;
    let _offset = EnvRestore::set("RYU_PORT_OFFSET", &offset.to_string());
    let server = serve_one_request(listener, "ghost-test-token");

    let delivered =
        post_to_shadow(r#"{"events":[]}"#).expect("the marker reaches the profile's Shadow port");
    assert_eq!(delivered, r#"{"ok":true}"#);
    let request = server.join().expect("profile-port server thread");
    assert!(
        request.starts_with("POST /ingest HTTP/1.1\r\n"),
        "{request}"
    );
    assert!(
        request.contains(&format!("\r\nHost: 127.0.0.1:{shifted_port}\r\n")),
        "{request}"
    );
    assert!(
        request.contains("\r\nAuthorization: Bearer ghost-test-token\r\n"),
        "{request}"
    );
}
