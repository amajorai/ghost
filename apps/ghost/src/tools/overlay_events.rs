// Action-event channel for the ghost-cursor overlay.
//
// When Ghost runs its pointer/keyboard tools it can (optionally) narrate each input
// action to a local overlay so a *visible* ghost cursor tracks what the agent is
// doing — the Codex "background computer use" affordance — without hijacking the
// user's real cursor (the AX-first path in ghost_hands already avoids that).
//
// The overlay is the Ryu Island's loopback listener. Its URL is passed in via the
// `RYU_GHOST_OVERLAY_URL` env var (Core injects it when it spawns the ghost sidecar).
// Every emit is strictly fire-and-forget: the payload (seq + ts) is built
// synchronously, the POST is detached onto the runtime with a 200 ms timeout, and
// any failure — unset URL, dead port, refused connection — is swallowed. An action
// must NEVER be delayed or failed by the overlay being absent.

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

/// Monotonic sequence number so the overlay can order/de-dup events per process.
static SEQ: AtomicU64 = AtomicU64::new(0);

/// Display identity supplied by Ryu Core through MCP request metadata. This is
/// deliberately out of the public Ghost tool arguments: models cannot claim to
/// be another agent or choose a different session title.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GhostCursorIdentity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_emoji: Option<String>,
}

impl GhostCursorIdentity {
    /// Bound untrusted MCP metadata before it is forwarded to Island's local
    /// control server. The cursor label is a display hint, never trusted state.
    pub fn sanitized(mut self) -> Option<Self> {
        self.agent_id = bounded_cursor_text(self.agent_id, 96);
        self.agent_name = bounded_cursor_text(self.agent_name, 80);
        self.agent_title = bounded_cursor_text(self.agent_title, 80);
        self.session_title = bounded_cursor_text(self.session_title, 160);
        self.avatar_emoji = bounded_cursor_text(self.avatar_emoji, 8);
        self.avatar_url = self.avatar_url.and_then(safe_cursor_avatar_url);

        (self.agent_id.is_some()
            || self.agent_name.is_some()
            || self.agent_title.is_some()
            || self.session_title.is_some()
            || self.avatar_emoji.is_some()
            || self.avatar_url.is_some())
        .then_some(self)
    }
}

fn bounded_cursor_text(value: Option<String>, limit: usize) -> Option<String> {
    let value = value?.trim().chars().take(limit).collect::<String>();
    (!value.is_empty()).then_some(value)
}

fn safe_cursor_avatar_url(value: String) -> Option<String> {
    let value = value.trim().to_owned();
    if value.is_empty() || value.len() > 20_000 {
        return None;
    }
    let lower = value.to_ascii_lowercase();
    let safe_data_image = [
        "data:image/png;base64,",
        "data:image/jpeg;base64,",
        "data:image/jpg;base64,",
        "data:image/webp;base64,",
        "data:image/gif;base64,",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix));
    (safe_data_image || lower.starts_with("https://") || lower.starts_with("http://"))
        .then_some(value)
}

tokio::task_local! {
    static CURSOR_IDENTITY: Option<GhostCursorIdentity>;
}

/// Scope trusted caller identity to one serialized MCP action. Event payloads
/// copy it synchronously before their fire-and-forget HTTP request is spawned.
pub async fn with_cursor_identity<F: Future>(
    identity: Option<GhostCursorIdentity>,
    future: F,
) -> F::Output {
    CURSOR_IDENTITY.scope(identity, future).await
}

/// The overlay URL, resolved once from the environment. `None` (the common case,
/// e.g. no Island running / not injected) makes every emit a no-op.
fn overlay_url() -> Option<&'static str> {
    static URL: OnceLock<Option<String>> = OnceLock::new();
    URL.get_or_init(|| {
        std::env::var("RYU_GHOST_OVERLAY_URL")
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    })
    .as_deref()
}

/// One narrated input action. `phase` is the lifecycle point:
/// `move` (heading to a target) · `down`/`up` (a click's press/release) ·
/// `type` · `scroll` · `done` (the tool finished).
#[derive(Debug, Clone, Serialize)]
pub struct GhostEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<GhostCursorIdentity>,
    pub intent: String,
    pub seq: u64,
    pub phase: &'static str,
    pub x: i32,
    pub y: i32,
    pub tool: String,
    pub ts: u128,
}

/// Build an event with the given fields (pure; used by the golden test).
fn build_event(
    seq: u64,
    phase: &'static str,
    x: i32,
    y: i32,
    tool: &str,
    intent: &str,
    ts: u128,
    identity: Option<GhostCursorIdentity>,
) -> GhostEvent {
    GhostEvent {
        identity,
        intent: bounded_intent(intent),
        seq,
        phase,
        x,
        y,
        tool: tool.to_owned(),
        ts,
    }
}

const MAX_INTENT_CHARS: usize = 72;

/// Keep the overlay chip readable and prevent an untrusted element name from
/// turning a click event into a huge always-on-top label.
fn bounded_intent(intent: &str) -> String {
    let trimmed = intent.trim();
    let mut chars = trimmed.chars();
    let prefix: String = chars.by_ref().take(MAX_INTENT_CHARS - 1).collect();
    if chars.next().is_some() {
        format!("{}…", prefix.trim_end())
    } else {
        trimmed.to_owned()
    }
}

fn default_intent(tool: &str) -> &'static str {
    match tool {
        "ghost_click" => "Clicking",
        "ghost_type" => "Typing",
        "ghost_scroll" => "Scrolling",
        "ghost_drag" => "Dragging",
        "ghost_hover" => "Hovering",
        "ghost_long_press" => "Holding",
        _ => "Working",
    }
}

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Narrate one input action to the overlay. Fire-and-forget: returns immediately;
/// the POST is detached and bounded to 200 ms. A no-op when no overlay URL is set.
pub fn emit(phase: &'static str, x: i32, y: i32, tool: &str) {
    emit_with_intent(phase, x, y, tool, default_intent(tool));
}

/// Narrate one input action with a short label shown beside the ghost cursor.
pub fn emit_with_intent(phase: &'static str, x: i32, y: i32, tool: &str, intent: &str) {
    let Some(url) = overlay_url() else {
        return;
    };
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let identity = CURSOR_IDENTITY.try_with(Clone::clone).unwrap_or_default();
    let event = build_event(seq, phase, x, y, tool, intent, now_millis(), identity);
    let Ok(body) = serde_json::to_string(&event) else {
        return;
    };
    let url = url.to_owned();
    // Detach: the action never awaits the network. The timeout bounds the spawned
    // task, not the caller.
    tokio::spawn(async move {
        let _ = tokio::time::timeout(Duration::from_millis(200), post(&url, &body)).await;
    });
}

/// Narrate the start of a press: the cursor heads to the target, then presses.
pub fn press_start(x: i32, y: i32, tool: &str) {
    press_start_with_intent(x, y, tool, default_intent(tool));
}

/// Narrate the start of a press with a target-aware intent label.
pub fn press_start_with_intent(x: i32, y: i32, tool: &str, intent: &str) {
    emit_with_intent("move", x, y, tool, intent);
    emit_with_intent("down", x, y, tool, intent);
}

/// Narrate the end of a press: release + the tool finishing.
pub fn press_end(x: i32, y: i32, tool: &str) {
    press_end_with_intent(x, y, tool, default_intent(tool));
}

/// Narrate the end of a press with the same target-aware intent label.
pub fn press_end_with_intent(x: i32, y: i32, tool: &str, intent: &str) {
    emit_with_intent("up", x, y, tool, intent);
    emit_with_intent("done", x, y, tool, intent);
}

/// Minimal loopback HTTP/1.1 POST — dependency-free (no reqwest in the ghost binary),
/// which suits a fire-and-forget notification to a known local endpoint. Errors are
/// returned so the caller's `timeout` can swallow them; nothing here can fail an action.
async fn post(url: &str, body: &str) -> std::io::Result<()> {
    let rest = url.strip_prefix("http://").unwrap_or(url);
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let mut stream = TcpStream::connect(authority).await?;
    let pid = std::process::id();
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\n\
         x-ghost-agent: {pid}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
        len = body.len(),
    );
    stream.write_all(req.as_bytes()).await?;
    stream.flush().await?;
    // The overlay's response is irrelevant; closing (via drop) is enough.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_payload_shape_is_stable() {
        // Golden: the exact JSON the overlay contract depends on. Fixed seq/ts so the
        // snapshot is deterministic.
        let event = build_event(
            7,
            "down",
            512,
            384,
            "ghost_click",
            "Click “Save”",
            1_700_000_000_000,
            None,
        );
        let value = serde_json::to_value(&event).expect("serializes");
        assert_eq!(
            value,
            serde_json::json!({
                "seq": 7,
                "phase": "down",
                "x": 512,
                "y": 384,
                "tool": "ghost_click",
                "intent": "Click “Save”",
                "ts": 1_700_000_000_000u128,
            })
        );
    }

    #[test]
    fn every_phase_serializes_to_its_literal() {
        for phase in ["move", "down", "up", "type", "scroll", "done"] {
            let event = build_event(0, phase, 0, 0, "ghost_type", "Typing", 0, None);
            let value = serde_json::to_value(&event).expect("serializes");
            assert_eq!(value["phase"], serde_json::json!(phase));
        }
    }

    #[test]
    fn intent_is_bounded_without_splitting_unicode() {
        let event = build_event(0, "down", 0, 0, "ghost_click", &"界".repeat(100), 0, None);
        let intent = event.intent;
        assert_eq!(intent.chars().count(), MAX_INTENT_CHARS);
        assert!(intent.ends_with('…'));
    }

    #[test]
    fn now_millis_reads_the_wall_clock() {
        assert!(now_millis() > 1_600_000_000_000);
    }

    #[test]
    fn event_carries_the_trusted_cursor_identity_when_available() {
        let identity = GhostCursorIdentity {
            agent_id: Some("@ryu/codex".into()),
            agent_name: Some("Codex".into()),
            agent_title: Some("Software engineer".into()),
            session_title: Some("Polish the computer-use cursor".into()),
            avatar_url: Some("data:image/png;base64,AA==".into()),
            avatar_emoji: None,
        };
        let event = build_event(
            1,
            "move",
            12,
            24,
            "ghost_click",
            "Click Save",
            0,
            Some(identity),
        );
        let value = serde_json::to_value(event).expect("serializes");
        assert_eq!(value["identity"]["agentId"], "@ryu/codex");
        assert_eq!(value["identity"]["agentName"], "Codex");
        assert_eq!(value["identity"]["agentTitle"], "Software engineer");
        assert_eq!(
            value["identity"]["sessionTitle"],
            "Polish the computer-use cursor"
        );
        assert_eq!(value["identity"]["avatarUrl"], "data:image/png;base64,AA==");
    }

    #[tokio::test]
    async fn cursor_identity_is_scoped_to_one_action_future() {
        let identity = GhostCursorIdentity {
            agent_id: Some("@ryu/codex".into()),
            agent_name: Some("Codex".into()),
            ..Default::default()
        };
        let scoped = with_cursor_identity(Some(identity.clone()), async {
            CURSOR_IDENTITY
                .try_with(Clone::clone)
                .expect("identity is available inside the action")
        })
        .await;
        assert_eq!(scoped, Some(identity));
        assert!(CURSOR_IDENTITY.try_with(Clone::clone).is_err());
    }

    #[test]
    fn cursor_identity_bounds_text_and_drops_unsafe_avatar_urls() {
        let identity = GhostCursorIdentity {
            agent_name: Some("x".repeat(100)),
            session_title: Some("t".repeat(200)),
            avatar_url: Some("javascript:alert(1)".into()),
            ..Default::default()
        }
        .sanitized()
        .expect("text identity remains useful");

        assert_eq!(identity.agent_name.as_deref().map(str::len), Some(80));
        assert_eq!(identity.session_title.as_deref().map(str::len), Some(160));
        assert_eq!(identity.avatar_url, None);
    }

    // Run under a runtime so the emit path is safe whether or not an overlay URL is set
    // in the environment (a set URL would `tokio::spawn` the detached POST).
    #[tokio::test]
    async fn emit_and_press_helpers_never_panic() {
        // With no overlay URL these are pure no-ops; the point is that narrating an
        // action must never fail or block the caller regardless of overlay presence.
        emit("move", 1, 2, "ghost_click");
        press_start(3, 4, "ghost_click");
        press_end(5, 6, "ghost_click");
    }
}
