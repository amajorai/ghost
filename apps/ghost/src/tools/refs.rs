//! Element refs: parsing `@eN` / `@<sid>:eN` and strict resolution against a stored
//! snapshot.
//!
//! A qualified ref (`@<sid>:eN`) resolves only against that snapshot. A bare ref with a
//! `snapshot` param resolves against that snapshot. A bare ref alone resolves against
//! the latest snapshot, and is refused when that snapshot was taken by a different
//! identified agent. The element is then found live through the platform backend
//! (path first, then identity, pid checked); a backend without live resolution falls
//! back to a fresh walk plus `refmap::resolve_entry`.

use std::time::{Duration, Instant};

use serde_json::json;

use ghost_eyes::{
    AXSnapshot, AXTree, ElementLocator, ErrorCode, GhostError, PlatformAXTree,
    PlatformWindowTracker, ResolvedElement, SnapshotOptions, WalkLimits, WindowTarget,
    WindowTracker,
};
use ghost_hands::focus_app;

use crate::refmap::{
    center_of, entry_identity, is_valid_snapshot_id, resolve_entry, SkeletonNode, Snapshot,
    SnapshotStore,
};

use super::overlay_events;

/// How long live resolution may take.
const RESOLVE_DEADLINE: Duration = Duration::from_secs(3);
/// Settle time after focusing the snapshot's app, as before strict refs.
const FOCUS_SETTLE: Duration = Duration::from_millis(120);
const RESNAPSHOT: &str = "run ghost_snapshot again and use the new ref";

/// A parsed ref: the snapshot it is pinned to (if qualified) and the local `@eN`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefTarget {
    pub snapshot: Option<String>,
    /// Normalized `@eN`.
    pub local: String,
}

fn malformed(s: &str) -> GhostError {
    let shown: String = s.chars().take(64).collect();
    GhostError::invalid_args(format!(
        "malformed ref '{shown}': expected @eN or @<snapshot_id>:eN with N >= 1"
    ))
}

/// Parse `@eN` or `@<sid>:eN`. The leading `@` is stripped before splitting on the
/// last `:`. N is a decimal in `1..=u32::MAX` without sign or leading zeros.
pub fn parse_ref(s: &str) -> Result<RefTarget, GhostError> {
    let trimmed = s.trim();
    let body = trimmed.strip_prefix('@').unwrap_or(trimmed);
    let (sid, local) = match body.rsplit_once(':') {
        Some((sid, local)) => (Some(sid), local),
        None => (None, body),
    };
    if let Some(sid) = sid {
        if !is_valid_snapshot_id(sid) {
            return Err(malformed(s));
        }
    }
    let digits = local.strip_prefix('e').ok_or_else(|| malformed(s))?;
    let valid_digits = !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && !digits.starts_with('0');
    let n: u32 = if valid_digits {
        digits.parse().map_err(|_| malformed(s))?
    } else {
        return Err(malformed(s));
    };
    Ok(RefTarget {
        snapshot: sid.map(str::to_string),
        local: format!("@e{n}"),
    })
}

/// Normalize a `snapshot` param (`<sid>`, also tolerating `@<sid>:`).
pub(crate) fn parse_snapshot_param(param: &str) -> Result<String, GhostError> {
    let trimmed = param.trim();
    let sid = trimmed.strip_prefix('@').unwrap_or(trimmed);
    let sid = sid.strip_suffix(':').unwrap_or(sid);
    if is_valid_snapshot_id(sid) {
        Ok(sid.to_string())
    } else {
        Err(GhostError::invalid_args(
            "invalid snapshot id: pass the snapshot_id returned by ghost_snapshot",
        ))
    }
}

/// The snapshot a ref is pinned to: the qualified sid, else the `snapshot` param, else
/// `None` (the latest). A qualified sid that differs from the param is INVALID_ARGS.
pub(crate) fn pinned_snapshot(
    target: &RefTarget,
    snapshot_param: Option<&str>,
) -> Result<Option<String>, GhostError> {
    let param = snapshot_param.map(parse_snapshot_param).transpose()?;
    match (&target.snapshot, param) {
        (Some(sid), Some(param)) if *sid != param => Err(GhostError::invalid_args(format!(
            "ref {} names snapshot {sid} but the snapshot param is {param}",
            target.local
        ))),
        (Some(sid), _) => Ok(Some(sid.clone())),
        (None, param) => Ok(param),
    }
}

/// A bare ref against the latest snapshot is refused when both the snapshot's creator
/// and the current caller are identified and differ.
pub(crate) fn check_bare_ref_caller(
    creator: Option<&str>,
    caller: Option<&str>,
    snapshot_id: &str,
) -> Result<(), GhostError> {
    match (creator, caller) {
        (Some(creator), Some(caller)) if creator != caller => Err(GhostError::stale_ref(
            "the latest snapshot was taken by another agent; a bare ref cannot target it",
        )
        .with_details(json!({ "reason": "bare_ref_other_caller", "snapshot_id": snapshot_id }))
        .with_suggestion(format!(
            "use the qualified form @<snapshot_id>:eN from your own ghost_snapshot (ref_prefix), or {RESNAPSHOT}"
        ))),
        _ => Ok(()),
    }
}

/// Load the pinned snapshot, or the latest. A missing or expired snapshot is STALE_REF.
pub(crate) fn load_snapshot(
    store: &SnapshotStore,
    pinned: Option<&str>,
) -> Result<Snapshot, GhostError> {
    let loaded = match pinned {
        Some(id) => store.load(id),
        None => store.load_latest(),
    };
    loaded.map_err(|_| {
        let message = match pinned {
            Some(id) => format!("snapshot {id} was not found or has expired"),
            None => "there is no current snapshot".to_string(),
        };
        GhostError::stale_ref(message)
            .with_details(json!({ "reason": "snapshot_expired" }))
            .with_suggestion(RESNAPSHOT)
    })
}

/// The locator for `entry` in `snapshot`: the snapshot's window (by pid and title when
/// known), the stored path and the stored identity.
pub(crate) fn locator_for(snapshot: &Snapshot, entry: &SkeletonNode) -> ElementLocator {
    let title = snapshot
        .window_title
        .clone()
        .filter(|t| !t.trim().is_empty());
    let window = match (snapshot.pid > 0, title) {
        (true, Some(title)) => WindowTarget::PidTitle {
            pid: snapshot.pid,
            title,
        },
        (true, None) => WindowTarget::Pid { pid: snapshot.pid },
        (false, _) => WindowTarget::Focused,
    };
    ElementLocator {
        pid: snapshot.pid,
        window,
        path: entry.path.clone(),
        identity: entry_identity(entry),
    }
}

/// A ref resolved to a live element.
// Until the action tools adopt strict refs, only the deprecated coordinate shim
// (`snapshot::resolve_ref`) reads it, through `center`.
#[allow(dead_code)]
pub struct ResolvedRef {
    pub snapshot_id: String,
    /// The ref was pinned to an explicit snapshot (qualified form or `snapshot` param).
    /// When false it was resolved against the latest snapshot, and callers report
    /// `ref_resolved_against: snapshot_id`.
    pub qualified: bool,
    pub entry: SkeletonNode,
    pub locator: ElementLocator,
    pub element: ResolvedElement,
    pub center: Option<(i32, i32)>,
}

/// Resolve `r` (`@eN` or `@<sid>:eN`) to a live element. See the module docs.
pub async fn resolve_ref(r: &str, snapshot_param: Option<&str>) -> Result<ResolvedRef, GhostError> {
    let target = parse_ref(r)?;
    let pinned = pinned_snapshot(&target, snapshot_param)?;
    let store = SnapshotStore::open().map_err(|e| GhostError::from_anyhow(&e))?;
    let snapshot = load_snapshot(&store, pinned.as_deref())?;
    if pinned.is_none() {
        check_bare_ref_caller(
            snapshot.creator.as_deref(),
            overlay_events::current_agent_id().as_deref(),
            &snapshot.id,
        )?;
    }
    let entry = snapshot.find_ref(&target.local).cloned().ok_or_else(|| {
        GhostError::stale_ref(format!(
            "{} is not in snapshot {}",
            target.local, snapshot.id
        ))
        .with_details(json!({ "reason": "ref_not_in_snapshot", "snapshot_id": snapshot.id }))
        .with_suggestion(RESNAPSHOT)
    })?;

    // Focus the snapshot's app so resolution reads the right window.
    if let Some(app) = &snapshot.app {
        focus_app(app);
        tokio::time::sleep(FOCUS_SETTLE).await;
    }

    let locator = locator_for(&snapshot, &entry);
    let ax = PlatformAXTree::new().map_err(|e| GhostError::from_anyhow(&e))?;
    let started = Instant::now();
    let element = match ax.resolve(&locator, RESOLVE_DEADLINE).await {
        Err(e) if e.code == ErrorCode::PlatformNotSupported => {
            let remaining = RESOLVE_DEADLINE.saturating_sub(started.elapsed());
            resolve_by_walk(&ax, &locator, &entry, remaining).await?
        }
        other => other?,
    };
    let center = center_of(&element.node);
    Ok(ResolvedRef {
        snapshot_id: snapshot.id,
        qualified: pinned.is_some(),
        entry,
        locator,
        element,
        center,
    })
}

/// Fallback for backends without live resolution: walk the window fresh, check the
/// pid, and resolve the stored entry in that tree.
async fn resolve_by_walk(
    ax: &PlatformAXTree,
    locator: &ElementLocator,
    entry: &SkeletonNode,
    remaining: Duration,
) -> Result<ResolvedElement, GhostError> {
    let opts = |target: WindowTarget| SnapshotOptions {
        target,
        limits: WalkLimits::platform_default().with_budget(remaining),
        activate_renderer: false,
    };
    let walked = match ax.snapshot(opts(locator.window.clone())).await {
        Err(e)
            if e.code == ErrorCode::PlatformNotSupported
                && locator.window != WindowTarget::Focused =>
        {
            ax.snapshot(opts(WindowTarget::Focused)).await?
        }
        other => other?,
    };
    let current_pid = current_pid(&walked).await;
    if locator.pid > 0 && current_pid > 0 && current_pid != locator.pid {
        return Err(GhostError::stale_ref(format!(
            "the focused app changed (snapshot pid {}, now {current_pid})",
            locator.pid
        ))
        .with_details(json!({
            "reason": "process_changed",
            "snapshot_pid": locator.pid,
            "current_pid": current_pid,
        }))
        .with_suggestion(RESNAPSHOT));
    }
    let (node, path, matched_by) = resolve_entry(&walked.root, entry)?;
    let state = node.states.clone().unwrap_or_default();
    Ok(ResolvedElement {
        node,
        path,
        matched_by,
        pid: current_pid,
        state,
    })
}

/// The walked window's pid, or the focused window's pid when the backend cannot tell.
async fn current_pid(walked: &AXSnapshot) -> i32 {
    if walked.pid > 0 {
        return walked.pid;
    }
    match PlatformWindowTracker::new() {
        Ok(tracker) => tracker.get_active_window().await.map_or(0, |w| w.pid),
        Err(_) => 0,
    }
}
