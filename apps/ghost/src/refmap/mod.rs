// Snapshot + element-ref store.
//
// Ported in spirit from vercel-labs/agent-desktop's snapshot/ref system: a `ghost_snapshot`
// captures the focused accessibility tree once, assigns stable `@eN` references to its
// interactive/named nodes in depth-first order, and keeps the whole walked tree sealed
// under Ghost's component key (the tree is on-screen text and field values; see
// `SnapshotStore`), so the next per-call `ghost mcp` process can resolve its refs. Subsequent action tools (`ghost_click`/`ghost_type`)
// then act on `@eN` instead of re-describing the element, which cuts token usage on dense
// apps and makes targeting deterministic.
//
// Re-identification is strict (see `resolve.rs` and `tools/refs.rs`): a ref stores the
// element's child-index path and identity (stable role, native id, identifier, name).
// Resolution tries the path first, then identity; values are never used, more than one
// candidate is `AMBIGUOUS_TARGET`, and a changed process or vanished element is
// `STALE_REF`, telling the agent to re-snapshot.

mod build;
mod resolve;
mod store;

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use ghost_eyes::{AXSnapshot, AXTreeNode, Bounds};

pub use build::render;
use build::{build_node, count_refs, find_ref_in};
pub use resolve::entry_identity;
// `reidentify`/`Resolved` remain the coordinate-only API (`crate::refmap::reidentify`).
pub(crate) use resolve::center_of;
pub use resolve::resolve_entry;
#[allow(unused_imports)]
pub use resolve::{reidentify, Resolved};
// `is_valid_snapshot_id` is re-exported for the ref parser (tools/refs.rs).
pub(crate) use store::is_valid_snapshot_id;
pub use store::{
    prune_expired_snapshots, remove_legacy_snapshots_at_startup, resolve_process_cipher,
    SnapshotStore, SNAPSHOT_PRUNE_INTERVAL,
};

/// How many snapshots to keep.
const MAX_SNAPSHOTS: usize = 10;

/// Monotonic per-process counter so two snapshots in the same millisecond get distinct ids.
static SNAPSHOT_SEQ: AtomicU64 = AtomicU64::new(0);

/// A node in a captured skeleton tree. Mirrors `AXTreeNode` but carries an optional
/// `@eN` ref and keeps every walked child (the skeleton *view* prunes by depth at
/// render time; the stored tree is whole so drill-down can re-render deeper).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkeletonNode {
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub ref_id: Option<String>,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Bounds>,
    pub children: Vec<SkeletonNode>,
    /// Child indices from the snapshot root, in walker order. Empty for the root and
    /// for snapshots sealed before paths were stored.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<u32>,
    /// Canonical role (`ghost_eyes::accessibility::roles`); absent on legacy entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_id: Option<String>,
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// A secure (password) field: `value` is never stored.
    #[serde(default, skip_serializing_if = "is_false")]
    pub secure: bool,
    /// The walk stopped below this node (depth, sibling, node or time budget).
    #[serde(default, skip_serializing_if = "is_false")]
    pub subtree_truncated: bool,
}

fn default_true() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// A captured snapshot: the tree plus the app/pid it was taken from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    pub pid: i32,
    pub created_ms: i64,
    pub tree: SkeletonNode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_title: Option<String>,
    /// The walk finished within every limit. `false` for legacy snapshots.
    #[serde(default)]
    pub complete: bool,
    #[serde(default)]
    pub truncated_nodes: u32,
    /// `RendererA11y` as its wire string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renderer: Option<String>,
    /// Agent id of the caller that took the snapshot, when it was identified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator: Option<String>,
}

impl Snapshot {
    /// Build a snapshot from a freshly captured AX tree, assigning `@eN` refs DFS.
    pub fn build(root: &AXTreeNode, app: Option<String>, pid: i32, now_ms: i64) -> Self {
        // The process id keeps ids unique across the per-call Ghost processes
        // that share the sealed snapshot directory.
        let seq = SNAPSHOT_SEQ.fetch_add(1, Ordering::Relaxed);
        let id = format!("s{now_ms}-{}{seq:06}", std::process::id());
        let mut counter: u32 = 0;
        let tree = build_node(root, &mut Vec::new(), &mut counter);
        Self {
            id,
            app,
            pid,
            created_ms: now_ms,
            tree,
            window_title: root.title.clone(),
            complete: false,
            truncated_nodes: 0,
            renderer: None,
            creator: None,
        }
    }

    /// Build a snapshot from a backend `AXSnapshot`, keeping its walk metadata.
    /// `pid` overrides the backend's pid (callers fill a 0 pid from the window tracker).
    pub fn from_ax(
        ax: &AXSnapshot,
        app: Option<String>,
        pid: i32,
        creator: Option<String>,
        now_ms: i64,
    ) -> Self {
        let mut snapshot = Self::build(&ax.root, app, pid, now_ms);
        snapshot.window_title = ax.window_title.clone().or(snapshot.window_title);
        snapshot.complete = ax.complete;
        snapshot.truncated_nodes = ax.stats.truncated_nodes;
        snapshot.renderer = serde_json::to_value(ax.renderer)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string));
        snapshot.creator = creator;
        snapshot
    }

    /// The prefix that qualifies this snapshot's bare refs: `@<sid>:`.
    pub fn ref_prefix(&self) -> String {
        format!("@{}:", self.id)
    }

    /// The total number of refs assigned in this snapshot.
    pub fn ref_count(&self) -> usize {
        count_refs(&self.tree)
    }

    /// Find a node by its `@eN` ref.
    pub fn find_ref(&self, ref_id: &str) -> Option<&SkeletonNode> {
        find_ref_in(&self.tree, ref_id)
    }
}

#[cfg(test)]
mod tests;
