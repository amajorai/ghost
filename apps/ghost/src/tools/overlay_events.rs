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

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

/// Monotonic sequence number so the overlay can order/de-dup events per process.
static SEQ: AtomicU64 = AtomicU64::new(0);

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
) -> GhostEvent {
    GhostEvent {
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
    let event = build_event(seq, phase, x, y, tool, intent, now_millis());
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
            let event = build_event(0, phase, 0, 0, "ghost_type", "Typing", 0);
            let value = serde_json::to_value(&event).expect("serializes");
            assert_eq!(value["phase"], serde_json::json!(phase));
        }
    }

    #[test]
    fn intent_is_bounded_without_splitting_unicode() {
        let event = build_event(0, "down", 0, 0, "ghost_click", &"界".repeat(100), 0);
        let intent = event.intent;
        assert_eq!(intent.chars().count(), MAX_INTENT_CHARS);
        assert!(intent.ends_with('…'));
    }

    #[test]
    fn now_millis_reads_the_wall_clock() {
        assert!(now_millis() > 1_600_000_000_000);
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
