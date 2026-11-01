//! MCP `tools/call` result envelopes.
//!
//! Successes keep their historical shape (one text block holding the JSON object, no
//! `structuredContent`). Errors carry the shared contract twice: once as the
//! human-readable `"Error: <CODE>: <message>"` text, which Core's `extract_mcp_json`
//! passes through unchanged, and once as `structuredContent.error`.

use serde_json::{json, Value};

use crate::errors;

/// The `result` of a successful tool call.
pub fn tool_success_result(content: &Value) -> Value {
    json!({
        "content": [{ "type": "text", "text": content.to_string() }]
    })
}

/// The `result` of a failed tool call:
/// `{content:[{type:"text", text:"Error: <CODE>: <message>"}], isError:true,
///   structuredContent:{ok:false, error:{code,message,disposition,suggestion?,details?}}}`.
pub fn tool_error_result(err: &anyhow::Error, tool: &str) -> Value {
    let g = errors::classify(err, tool);
    json!({
        "content": [{ "type": "text", "text": format!("Error: {g}") }],
        "isError": true,
        "structuredContent": { "ok": false, "error": g.to_json() },
    })
}
