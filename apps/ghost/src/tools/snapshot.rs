use anyhow::Result;
use serde_json::{json, Value};

use ghost_eyes::{
    AXTree, GhostError, PlatformAXTree, PlatformWindowTracker, SnapshotOptions, WalkLimits,
    WindowTarget, WindowTracker,
};
use ghost_hands::focus_app;

use crate::refmap::{render, Snapshot, SnapshotStore};

use super::refs::{load_snapshot, parse_ref, pinned_snapshot};
use super::{int_param, nonblank_str_param, overlay_events, str_param};

const DRILL_SUGGESTION: &str =
    "Act on a ref with ghost_click/ghost_type {\"ref\":\"@<snapshot_id>:eN\"} (the qualified form, ref_prefix + eN, is preferred over a bare @eN), or expand a container with ghost_snapshot {\"root\":\"@<snapshot_id>:eN\"}.";

const INCOMPLETE_SUGGESTION: &str =
    "The walk hit a limit, so this tree is incomplete: nodes marked subtree_truncated were not fully walked. Drill down with ghost_snapshot {\"root\":\"@<snapshot_id>:eN\"} on the container you need, or snapshot a narrower window with {\"window\":\"<title>\"}.";

/// Capture a fresh skeleton of the focused window (assigning stable `@eN` refs), or
/// — when `root` is given — re-render a deeper view of a container from an existing
/// snapshot. Drill-down does not walk deeper than the original capture (bounded by the
/// platform AX walk: ~6 levels on Windows/Linux, ~25 on macOS); to reach elements below
/// that, snapshot the relevant app/window directly.
pub async fn ghost_snapshot(params: Value) -> Result<Value> {
    if let Some(app_name) = str_param(&params, "app") {
        focus_app(app_name);
        tokio::time::sleep(tokio::time::Duration::from_millis(120)).await;
    }

    let depth = int_param(&params, "depth", 3).clamp(0, 10) as u32;
    let store = SnapshotStore::open()?;

    // Drill-down: re-render a container from a stored snapshot, no re-capture.
    if let Some(root) = str_param(&params, "root") {
        return drill_down(&store, root, str_param(&params, "snapshot"), depth).map_err(Into::into);
    }

    // Fresh capture. The walker computes its own deadline, so the budget starts
    // after the optional focus above.
    let target = nonblank_str_param(&params, "window").map_or(WindowTarget::Focused, |title| {
        WindowTarget::FocusedTitle {
            title: title.to_string(),
        }
    });
    let ax = PlatformAXTree::new()?;
    let captured = ax
        .snapshot(SnapshotOptions {
            target,
            limits: WalkLimits::platform_default(),
            activate_renderer: true,
        })
        .await?;

    // The backend may not know the pid or app (pid 0); the focused window fills them.
    let active = match PlatformWindowTracker::new() {
        Ok(tracker) => tracker.get_active_window().await,
        Err(_) => None,
    };
    let pid = if captured.pid > 0 {
        captured.pid
    } else {
        active.as_ref().map_or(0, |w| w.pid)
    };
    let app_name = captured
        .app_name
        .clone()
        .or_else(|| active.as_ref().map(|w| w.app_name.clone()));

    let now_ms = chrono::Utc::now().timestamp_millis();
    let creator = overlay_events::current_agent_id();
    let snapshot = Snapshot::from_ax(&captured, app_name.clone(), pid, creator, now_ms);
    store.save(&snapshot)?;

    let suggestion = if snapshot.complete {
        DRILL_SUGGESTION.to_string()
    } else {
        format!("{INCOMPLETE_SUGGESTION} {DRILL_SUGGESTION}")
    };
    Ok(json!({
        "snapshot_id": snapshot.id,
        "ref_prefix": snapshot.ref_prefix(),
        "app": app_name,
        "pid": pid,
        "window": snapshot.window_title,
        "ref_count": snapshot.ref_count(),
        "complete": snapshot.complete,
        "truncated_nodes": snapshot.truncated_nodes,
        "accessibility_enabled": snapshot.renderer,
        "tree": render(&snapshot.tree, depth),
        "suggestion": suggestion,
    }))
}

/// Re-render the subtree under `root` (`@eN` or `@<sid>:eN`) from a stored snapshot.
fn drill_down(
    store: &SnapshotStore,
    root: &str,
    snapshot_param: Option<&str>,
    depth: u32,
) -> Result<Value, GhostError> {
    let target = parse_ref(root)?;
    let pinned = pinned_snapshot(&target, snapshot_param)?;
    let snapshot = load_snapshot(store, pinned.as_deref())?;
    let node = snapshot.find_ref(&target.local).ok_or_else(|| {
        GhostError::stale_ref(format!(
            "ref {} not found in snapshot {}",
            target.local, snapshot.id
        ))
        .with_details(json!({ "reason": "ref_not_in_snapshot", "snapshot_id": snapshot.id }))
        .with_suggestion("run ghost_snapshot again and use the new ref")
    })?;
    Ok(json!({
        "snapshot_id": snapshot.id,
        "ref_prefix": snapshot.ref_prefix(),
        "root": target.local,
        "tree": render(node, depth),
        "suggestion": DRILL_SUGGESTION,
    }))
}
