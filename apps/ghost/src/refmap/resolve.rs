//! Strict re-identification of a stored ref in a freshly captured tree.
//!
//! The stored entry becomes an `ElementIdentity`, and `resolve_in_tree` (in eyes)
//! tries the stored path first, then identity. Values never locate an element, and
//! more than one candidate is `AMBIGUOUS_TARGET`, never a guess.

use ghost_eyes::accessibility::{resolve, roles};
use ghost_eyes::{AXTreeNode, ElementIdentity, GhostError, MatchBy};

use super::SkeletonNode;

/// Outcome of resolving a ref against the currently focused tree.
// Kept for coordinate-only callers; the tools now resolve through `tools::refs`.
#[allow(dead_code)]
pub enum Resolved {
    /// The element was re-identified; click at this screen-space center.
    At { x: i32, y: i32 },
    /// The element could not be re-identified — the agent should re-snapshot. The
    /// message starts with its code (`STALE_REF:`, `AMBIGUOUS_TARGET:`, …).
    Stale(String),
}

/// The identity a stored entry pins. A legacy entry without `role_key` falls back to
/// its normalized stored role; a secure entry contributes no value.
pub fn entry_identity(entry: &SkeletonNode) -> ElementIdentity {
    let role_key = entry
        .role_key
        .clone()
        .filter(|k| !k.is_empty())
        .unwrap_or_else(|| roles::normalize_legacy_role(&entry.role));
    let value = if entry.secure {
        None
    } else {
        entry.value.as_deref()
    };
    ElementIdentity::from_parts(
        &role_key,
        entry.name.as_deref(),
        value,
        entry.identifier.as_deref(),
        entry.native_id.as_deref(),
        entry.bounds.clone(),
    )
}

/// Resolve a stored entry in `current` (a tree rooted at the same window as the
/// snapshot). Returns the matched node at depth 0 (children dropped, redacted), its
/// path from `current`, and how it matched.
pub fn resolve_entry(
    current: &AXTreeNode,
    entry: &SkeletonNode,
) -> Result<(AXTreeNode, Vec<u32>, MatchBy), GhostError> {
    let identity = entry_identity(entry);
    let found = resolve::resolve_in_tree(current, &identity, &entry.path)?;
    Ok((depth0(found.node), found.path, found.matched_by))
}

/// A depth-0, redacted copy of `node`.
pub(crate) fn depth0(node: &AXTreeNode) -> AXTreeNode {
    let mut copy = AXTreeNode {
        children: Vec::new(),
        ..node.clone()
    };
    copy.redact();
    copy
}

/// The screen-space center of `node`, if it has bounds.
pub(crate) fn center_of(node: &AXTreeNode) -> Option<(i32, i32)> {
    node.bounds.as_ref().map(|b| {
        (
            b.x.saturating_add(i32::try_from(b.width / 2).unwrap_or(i32::MAX)),
            b.y.saturating_add(i32::try_from(b.height / 2).unwrap_or(i32::MAX)),
        )
    })
}

/// Re-identify a stored ref entry in a freshly captured tree, returning its current
/// center. `entry` is the `SkeletonNode` the ref pointed at in the snapshot. Kept as
/// a thin wrapper over [`resolve_entry`] for callers that only need coordinates.
#[allow(dead_code)]
pub fn reidentify(current_root: &AXTreeNode, entry: &SkeletonNode) -> Resolved {
    let label = entry.ref_id.as_deref().unwrap_or("element");
    match resolve_entry(current_root, entry) {
        Ok((node, _, _)) => match center_of(&node) {
            Some((x, y)) => Resolved::At { x, y },
            None => Resolved::Stale(
                GhostError::stale_ref(format!(
                    "{label} matched but has no bounds to click; re-run ghost_snapshot"
                ))
                .to_string(),
            ),
        },
        Err(err) => Resolved::Stale(format!("{err} ({label}); re-run ghost_snapshot")),
    }
}
