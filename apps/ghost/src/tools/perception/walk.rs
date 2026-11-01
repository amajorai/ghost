//! Accessibility-tree helpers for the perception tools: actionable/editable
//! role checks, layout-container tunnelling, matching and text collection.
//!
//! Every node is serialized through `redact::node_json`, and every query
//! match goes through `ghost_eyes::accessibility::find`, so a secure node's
//! value is never emitted and never compared.

use serde_json::{json, Value};

use ghost_eyes::accessibility::{find, roles};
use ghost_eyes::{AXTreeNode, FindOptions};

use super::redact::{node_json_with, safe_value};

/// Upper bound on `ghost_find` AX results.
pub(super) const FIND_LIMIT: usize = 50;

pub(super) fn node_to_value(node: &AXTreeNode) -> Value {
    node_json_with(node, json!({ "actionable": is_actionable(node) }))
}

/// Canonical actionable roles, plus the legacy English substring fallbacks
/// for nodes whose role does not normalize (e.g. localized Windows roles).
pub(super) fn is_actionable(node: &AXTreeNode) -> bool {
    if roles::is_actionable_role(&node.stable_role()) {
        return true;
    }
    let r = node.role.to_lowercase();
    r.contains("button")
        || r.contains("link")
        || r.contains("textfield")
        || r.contains("text field")
        || r.contains("checkbox")
        || r.contains("combo")
        || r.contains("slider")
        || r.contains("tab")
        || r.contains("menu")
        || r.contains("cell")
        || r.contains("row")
}

pub(super) fn is_editable(node: &AXTreeNode) -> bool {
    if roles::is_editable_role(&node.stable_role()) {
        return true;
    }
    let r = node.role.to_lowercase();
    r.contains("text") || r.contains("field") || r.contains("edit") || r.contains("combo")
}

/// First node with `focused == true` in walker (pre-)order.
fn first_focused(node: &AXTreeNode) -> Option<&AXTreeNode> {
    if node.focused {
        return Some(node);
    }
    node.children.iter().find_map(first_focused)
}

/// The focused element (falling back to the root), redacted.
pub(super) fn focused_element(tree: &AXTreeNode) -> Value {
    let node = first_focused(tree).unwrap_or(tree);
    node_json_with(
        node,
        json!({ "editable": is_editable(node), "focused": node.focused }),
    )
}

/// `ghost_inspect` output for one element, redacted.
pub(super) fn inspect_json(el: &AXTreeNode) -> Value {
    node_json_with(
        el,
        json!({
            "actionable":     is_actionable(el),
            "editable":       is_editable(el),
            "enabled":        el.enabled,
            "focused":        el.focused,
            "hidden":         el.hidden,
            "children_count": el.children.len(),
        }),
    )
}

/// `ghost_element_at` output for one element, redacted.
pub(super) fn element_at_json(x: i32, y: i32, el: &AXTreeNode) -> Value {
    node_json_with(el, json!({ "x": x, "y": y }))
}

/// Returns true for layout-only container roles that cost 0 depth budget.
/// These are structural nodes with no semantic content of their own.
pub(super) fn is_layout_container(node: &AXTreeNode) -> bool {
    matches!(
        node.role.to_lowercase().as_str(),
        "group"
            | "axgroup"
            | "scrollarea"
            | "axscrollarea"
            | "splitgroup"
            | "axsplitgroup"
            | "layoutitem"
            | "axlayoutitem"
            | "box"
            | "generic"
            | "none"
            | ""
    )
}

fn child_depth(node: &AXTreeNode, depth: u32) -> u32 {
    // Layout containers don't consume depth budget — tunnel through them.
    if is_layout_container(node) {
        depth
    } else {
        depth + 1
    }
}

pub(super) fn collect_interactive(node: &AXTreeNode, depth: u32, max_depth: u32) -> Vec<Value> {
    let mut out = vec![];
    if is_actionable(node) {
        out.push(node_to_value(node));
    }
    if depth < max_depth {
        let next = child_depth(node, depth);
        for child in &node.children {
            out.extend(collect_interactive(child, next, max_depth));
        }
    }
    out
}

/// Tunnelled depth of the node at `path` (layout containers cost nothing).
fn depth_at_path(root: &AXTreeNode, path: &[u32]) -> Option<u32> {
    let mut node = root;
    let mut depth = 0;
    for &idx in path {
        depth = child_depth(node, depth);
        node = node.children.get(usize::try_from(idx).ok()?)?;
    }
    Some(depth)
}

/// Every node within `max_depth` (tunnelled), in pre-order.
fn nodes_within<'a>(
    node: &'a AXTreeNode,
    depth: u32,
    max_depth: u32,
    out: &mut Vec<&'a AXTreeNode>,
) {
    if depth > max_depth {
        return;
    }
    out.push(node);
    let next = child_depth(node, depth);
    for child in &node.children {
        nodes_within(child, next, max_depth, out);
    }
}

/// `ghost_find` AX matching. The free-text `query` goes through
/// `find::find_all` (title, role, identifier and non-secure value); `role`,
/// `id` and `class` are substring filters on role and identifier only.
pub(super) fn find_elements(
    node: &AXTreeNode,
    query: &str,
    role: &str,
    id: &str,
    class: &str,
    max_depth: u32,
    out: &mut Vec<Value>,
) {
    let candidates: Vec<&AXTreeNode> = if query.trim().is_empty() {
        let mut all = Vec::new();
        nodes_within(node, 0, max_depth, &mut all);
        all
    } else {
        find::find_all(node, query, FindOptions::default(), usize::MAX)
            .into_iter()
            .filter(|(path, _)| depth_at_path(node, path).is_some_and(|d| d <= max_depth))
            .map(|(_, n)| n)
            .collect()
    };
    let role = role.to_lowercase();
    let id = id.to_lowercase();
    let class = class.to_lowercase();
    for n in candidates {
        if out.len() >= FIND_LIMIT {
            break;
        }
        let role_lc = n.role.to_lowercase();
        let ident_lc = n.identifier.as_deref().unwrap_or("").to_lowercase();
        let r_ok = role.is_empty() || role_lc.contains(&role);
        let i_ok = id.is_empty() || ident_lc.contains(&id);
        // dom_class: AX trees don't expose CSS classes; best-effort match against identifier
        let c_ok = class.is_empty() || ident_lc.contains(&class);
        if r_ok && i_ok && c_ok {
            out.push(node_to_value(n));
        }
    }
}

/// First subtree whose role, title or (non-secure) value contains `query`.
pub(super) fn find_node_ref<'a>(node: &'a AXTreeNode, query: &str) -> Option<&'a AXTreeNode> {
    let opts = FindOptions {
        match_identifier: false,
        match_value: true,
    };
    find::find_all(node, query, opts, 1)
        .into_iter()
        .next()
        .map(|(_, n)| n)
}

/// Text lines of a node and its descendants. A secure node's value is skipped.
pub(super) fn collect_text(node: &AXTreeNode, max_depth: u32, depth: u32, out: &mut Vec<String>) {
    if depth > max_depth {
        return;
    }
    if let Some(v) = safe_value(node) {
        if !v.trim().is_empty() {
            out.push(v.trim().to_string());
        }
    }
    if let Some(t) = &node.title {
        if !t.trim().is_empty() && !out.contains(t) {
            out.push(t.trim().to_string());
        }
    }
    for child in &node.children {
        collect_text(child, max_depth, depth + 1, out);
    }
}

/// `ghost_read` lines: the whole tree, or the first subtree matching `query`.
pub(super) fn read_lines(tree: &AXTreeNode, query: &str, max_depth: u32) -> Vec<String> {
    let subtree = if query.trim().is_empty() {
        Some(tree)
    } else {
        find_node_ref(tree, query)
    };
    let mut texts = vec![];
    if let Some(node) = subtree {
        collect_text(node, max_depth, 0, &mut texts);
    }
    texts
}
