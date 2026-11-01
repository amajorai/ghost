//! Secure-value redaction for perception output.
//!
//! The eyes walkers already set `value = None` on secure nodes (password
//! fields). This module is the consumer-side guard: every perception tool
//! serializes a node through `node_json`, which never emits a secure node's
//! value and marks it with `secure: true, value_withheld: true` instead.

use serde_json::{json, Map, Value};

use ghost_eyes::AXTreeNode;

/// True when the node, or its read-back state, is marked secure.
pub(super) fn is_secure(node: &AXTreeNode) -> bool {
    node.secure || node.states.as_ref().is_some_and(|s| s.secure)
}

/// The node's value, or `None` for a secure node regardless of what the
/// backend reported.
pub(super) fn safe_value(node: &AXTreeNode) -> Option<&str> {
    if is_secure(node) {
        None
    } else {
        node.value.as_deref()
    }
}

/// `{role, title, value, identifier, bounds}` plus `secure` and
/// `value_withheld` for a secure node, whose `value` is always `null`.
pub(super) fn node_json(node: &AXTreeNode) -> Value {
    let mut out = Map::new();
    out.insert("role".into(), json!(node.role));
    out.insert("title".into(), json!(node.title));
    out.insert("value".into(), json!(safe_value(node)));
    out.insert("identifier".into(), json!(node.identifier));
    out.insert("bounds".into(), json!(node.bounds));
    if is_secure(node) {
        out.insert("secure".into(), Value::Bool(true));
        out.insert("value_withheld".into(), Value::Bool(true));
    }
    Value::Object(out)
}

/// `node_json` with extra fields merged in (extra keys never override the
/// redacted `value`).
pub(super) fn node_json_with(node: &AXTreeNode, extra: Value) -> Value {
    let mut base = node_json(node);
    if let (Some(map), Value::Object(extra)) = (base.as_object_mut(), extra) {
        for (key, val) in extra {
            if key == "value" || key == "value_withheld" || key == "secure" {
                continue;
            }
            map.insert(key, val);
        }
    }
    base
}
