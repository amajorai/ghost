//! Skeleton construction (ref assignment) and compact rendering.

use serde_json::{json, Value};

use ghost_eyes::accessibility::roles;
use ghost_eyes::AXTreeNode;

use super::SkeletonNode;

/// A node deserves a ref if it is directly actionable, or if it is a named container
/// with children (a useful `--root` drill-down target).
fn deserves_ref(node: &AXTreeNode) -> bool {
    if roles::is_actionable_role(&node.stable_role()) || is_actionable_english(node) {
        return true;
    }
    node.title.as_deref().is_some_and(|t| !t.is_empty()) && !node.children.is_empty()
}

/// English fallbacks for backends whose roles are not canonical (Windows reports
/// localized control-type names).
fn is_actionable_english(node: &AXTreeNode) -> bool {
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
        || r.contains("edit")
}

/// Builds the skeleton of `node`, which sits at `path` under the snapshot root.
/// Child paths follow the walker order, so a stored path addresses the same native
/// child again. A secure node's value is never copied, even if a walker left one.
pub(super) fn build_node(
    node: &AXTreeNode,
    path: &mut Vec<u32>,
    counter: &mut u32,
) -> SkeletonNode {
    // Pre-order: a parent gets its ref before its children, so refs read in DFS order.
    let ref_id = if deserves_ref(node) {
        *counter += 1;
        Some(format!("@e{counter}"))
    } else {
        None
    };
    let mut children = Vec::with_capacity(node.children.len());
    for (index, child) in node.children.iter().enumerate() {
        path.push(u32::try_from(index).unwrap_or(u32::MAX));
        children.push(build_node(child, path, counter));
        path.pop();
    }
    let secure = node.secure || node.states.as_ref().is_some_and(|s| s.secure);
    SkeletonNode {
        ref_id,
        role: node.role.clone(),
        name: node.title.clone(),
        value: if secure { None } else { node.value.clone() },
        identifier: node.identifier.clone(),
        bounds: node.bounds.clone(),
        children,
        path: path.clone(),
        role_key: node.role_key.clone().filter(|k| !k.is_empty()),
        native_id: node.native_id.clone().filter(|n| !n.is_empty()),
        enabled: node.enabled,
        secure,
        subtree_truncated: node.subtree_truncated,
    }
}

pub(super) fn count_refs(node: &SkeletonNode) -> usize {
    let mut n = usize::from(node.ref_id.is_some());
    for c in &node.children {
        n += count_refs(c);
    }
    n
}

pub(super) fn find_ref_in<'a>(node: &'a SkeletonNode, ref_id: &str) -> Option<&'a SkeletonNode> {
    if node.ref_id.as_deref() == Some(ref_id) {
        return Some(node);
    }
    for c in &node.children {
        if let Some(found) = find_ref_in(c, ref_id) {
            return Some(found);
        }
    }
    None
}

/// Render a skeleton node to compact JSON, pruning at `max_depth` but always reporting
/// `children_count` so the agent knows where it can drill down with `root`.
pub fn render(node: &SkeletonNode, max_depth: u32) -> Value {
    render_inner(node, 0, max_depth)
}

fn render_inner(node: &SkeletonNode, depth: u32, max_depth: u32) -> Value {
    let mut obj = serde_json::Map::new();
    if let Some(r) = &node.ref_id {
        obj.insert("ref".into(), json!(r));
    }
    obj.insert("role".into(), json!(node.role));
    if let Some(n) = &node.name {
        obj.insert("name".into(), json!(n));
    }
    if node.secure {
        obj.insert("secure".into(), json!(true));
        obj.insert("value_withheld".into(), json!(true));
    } else if let Some(v) = &node.value {
        obj.insert("value".into(), json!(v));
    }
    if let Some(b) = &node.bounds {
        obj.insert(
            "bounds".into(),
            json!({ "x": b.x, "y": b.y, "w": b.width, "h": b.height }),
        );
    }
    if node.subtree_truncated {
        obj.insert("subtree_truncated".into(), json!(true));
    }
    if !node.children.is_empty() {
        obj.insert("children_count".into(), json!(node.children.len()));
        if depth < max_depth {
            let kids: Vec<Value> = node
                .children
                .iter()
                .map(|c| render_inner(c, depth + 1, max_depth))
                .collect();
            obj.insert("children".into(), json!(kids));
        }
    }
    Value::Object(obj)
}
