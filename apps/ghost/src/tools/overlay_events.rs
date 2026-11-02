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

/// agent_id of the trusted cursor identity scoped to the current MCP call, if any.
pub(crate) fn current_agent_id() -> Option<String> {
    CURSOR_IDENTITY
        .try_with(|identity| identity.as_ref().and_then(|i| i.agent_id.clone()))
        .ok()
        .flatten()
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
    /// English prose label, kept for Island builds that predate `kind`.
    pub intent: String,
    /// Stable intent kind (see [`GhostIntent`]); Island renders it in the
    /// user's language.
    pub kind: &'static str,
    /// The element a click targets, shown verbatim inside the translated label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
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
    intent: &GhostIntent,
    ts: u128,
    identity: Option<GhostCursorIdentity>,
) -> GhostEvent {
    GhostEvent {
        identity,
        intent: bounded_intent(&intent.prose()),
        kind: intent.kind,
        target: intent
            .target
            .as_deref()
            .map(bounded_intent)
            .filter(|target| !target.is_empty()),
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

/// What the agent is doing, as a stable kind plus an optional target. Ghost
/// sends the kind (not English prose) so Island can label the cursor in the
/// user's language; `prose()` is the English fallback for older Island builds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhostIntent {
    kind: &'static str,
    target: Option<String>,
}

/// Every kind Ghost emits, with its English prose. Island's
/// `GHOST_INTENT_KINDS` mirrors this list.
const INTENT_KINDS: [(&str, &str); 8] = [
    ("click", "Click"),
    ("clicking", "Clicking"),
    ("typing", "Typing"),
    ("scrolling", "Scrolling"),
    ("dragging", "Dragging"),
    ("hovering", "Hovering"),
    ("holding", "Holding"),
    ("working", "Working"),
];

impl GhostIntent {
    /// The default intent for a tool with no more specific target.
    pub fn for_tool(tool: &str) -> Self {
        let kind = match tool {
            "ghost_click" => "clicking",
            "ghost_type" => "typing",
            "ghost_scroll" => "scrolling",
            "ghost_drag" => "dragging",
            "ghost_hover" => "hovering",
            "ghost_long_press" => "holding",
            _ => "working",
        };
        Self { kind, target: None }
    }

    /// A click, naming the element when one is known. The full query or
    /// selector stays out of the protocol; only a trimmed label travels.
    pub fn click(target: Option<&str>) -> Self {
        Self {
            kind: "click",
            target: target
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
        }
    }

    #[cfg(test)]
    pub fn kind(&self) -> &'static str {
        self.kind
    }

    /// English label for Island builds that do not read `kind` yet.
    pub fn prose(&self) -> String {
        match (&self.target, self.kind) {
            (Some(target), "click") => format!("Click “{target}”"),
            _ => INTENT_KINDS
                .iter()
                .find(|(kind, _)| *kind == self.kind)
                .map_or("Working", |(_, prose)| prose)
                .to_owned(),
        }
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
    emit_with_intent(phase, x, y, tool, &GhostIntent::for_tool(tool));
}

/// Narrate one input action with the intent shown beside the ghost cursor.
pub fn emit_with_intent(phase: &'static str, x: i32, y: i32, tool: &str, intent: &GhostIntent) {
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
    press_start_with_intent(x, y, tool, &GhostIntent::for_tool(tool));
}

/// Narrate the start of a press with a target-aware intent.
pub fn press_start_with_intent(x: i32, y: i32, tool: &str, intent: &GhostIntent) {
    emit_with_intent("move", x, y, tool, intent);
    emit_with_intent("down", x, y, tool, intent);
}

/// Narrate the end of a press: release + the tool finishing.
pub fn press_end(x: i32, y: i32, tool: &str) {
    press_end_with_intent(x, y, tool, &GhostIntent::for_tool(tool));
}

/// Narrate the end of a press with the same target-aware intent.
pub fn press_end_with_intent(x: i32, y: i32, tool: &str, intent: &GhostIntent) {
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
#[path = "overlay_events_tests.rs"]
mod tests;
