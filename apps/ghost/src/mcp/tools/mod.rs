// All 31 MCP tool definitions — ported from ghost-os MCPTools.swift, plus
// ghost_snapshot (snapshot/ref model, ported in spirit from vercel-labs/agent-desktop).
// These are the agent-facing contracts: names, descriptions, and parameter schemas.

mod actions;
mod other;
mod perception;

use serde_json::{json, Value};

/// Every tool definition, in the order `tools/list` returns them.
pub fn definitions() -> Vec<Value> {
    let mut defs = perception::definitions();
    defs.extend(actions::definitions());
    defs.extend(other::definitions());
    defs
}

// ─── Schema helpers ───────────────────────────────────────────────────────────

fn tool(
    name: &str,
    description: &str,
    props: &[(&str, &str, &str, bool)],
    required: &[&str],
) -> Value {
    let mut properties = serde_json::Map::new();
    for (pname, ptype, pdesc, _) in props {
        properties.insert(
            pname.to_string(),
            json!({ "type": ptype, "description": pdesc }),
        );
    }
    let mut schema = json!({ "type": "object", "properties": properties });
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    json!({
        "name": name,
        "description": description,
        "inputSchema": schema,
    })
}

fn tool_rich(name: &str, description: &str, schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": schema,
    })
}

#[cfg(test)]
mod tests;
