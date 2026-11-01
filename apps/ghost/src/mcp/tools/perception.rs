//! Perception tool definitions (context, state, find, read, inspect, element-at, screenshot, snapshot) plus the journal marker.

use serde_json::{json, Value};

use super::{tool, tool_rich};

pub(super) fn definitions() -> Vec<Value> {
    vec![
        // Perception (8)
        tool("ghost_context",
            "Get orientation: focused app, window title, URL (browsers), focused element, and interactive elements. Call this before acting on any app.",
            &[("app", "string", "App name to get context for. If omitted, returns focused app.", false)],
            &[]),

        tool("ghost_state",
            "List all running apps and their windows with titles, positions, and sizes.",
            &[("app", "string", "Filter to a specific app.", false)],
            &[]),

        tool_rich("ghost_find",
            "Find elements in any app. Returns matching elements with role, name, position, and available actions.",
            json!({
                "type": "object",
                "properties": {
                    "query":      { "type": "string",  "description": "Text to search for (matches title, value, identifier, description)." },
                    "role":       { "type": "string",  "description": "AX role filter (e.g. AXButton, AXTextField, AXLink)." },
                    "dom_id":     { "type": "string",  "description": "Find by DOM id (web apps, bypasses depth limits)." },
                    "dom_class":  { "type": "string",  "description": "Find by CSS class." },
                    "identifier": { "type": "string",  "description": "Find by AX identifier." },
                    "app":        { "type": "string",  "description": "Which app to search in." },
                    "depth":      { "type": "integer", "description": "Max search depth (default: 25, max: 100)." }
                }
            })),

        tool("ghost_read",
            "Read text content from screen. Returns concatenated text from the element subtree.",
            &[
                ("app",   "string",  "Which app to read from.", false),
                ("query", "string",  "Narrow to specific element.", false),
                ("depth", "integer", "How deep to read (default: 25).", false),
            ],
            &[]),

        tool_rich("ghost_inspect",
            "Full metadata about one element. Call this before acting on something you're unsure about. Returns role, title, position, size, actionable status, supported actions, editable, DOM id, and more.",
            json!({
                "type": "object",
                "properties": {
                    "query":     { "type": "string", "description": "Element to inspect (text/name)." },
                    "dom_id":    { "type": "string", "description": "Find by DOM id (CDP)." },
                    "dom_class": { "type": "string", "description": "Find by CSS class (CDP)." }
                }
            })),

        tool_rich("ghost_element_at",
            "What element is at this screen position? Bridges screenshots and accessibility tree.",
            json!({
                "type": "object",
                "properties": {
                    "x": { "type": "number", "description": "X coordinate." },
                    "y": { "type": "number", "description": "Y coordinate." }
                },
                "required": ["x", "y"]
            })),

        tool("ghost_screenshot",
            "Take a screenshot for visual debugging. Returns base64 PNG.",
            &[
                ("full_resolution","boolean", "Native resolution instead of 1280px resize (default: false).", false),
            ],
            &[]),

        tool_rich("ghost_snapshot",
            "Capture a compact skeleton of the focused window with element refs and children_count per node. Token-efficient orientation: snapshot once, then act on refs (ghost_click/ghost_type with {\"ref\":...}) instead of re-describing elements. The tree shows bare @eN refs; the response's ref_prefix (\"@<snapshot_id>:\") turns them into the preferred qualified form @<snapshot_id>:eN, which always resolves against this snapshot. A bare @eN resolves only against the latest snapshot. complete:false means the walk hit a depth, sibling, node or time limit: nodes marked subtree_truncated have unseen children, so drill in with 'root' (or snapshot that app/window directly) instead of assuming an element is absent. Secure fields carry value_withheld:true and never show their value.",
            json!({
                "type": "object",
                "properties": {
                    "app":      { "type": "string",  "description": "App to focus and snapshot first. If omitted, uses the currently focused window." },
                    "window":   { "type": "string",  "description": "Snapshot the window of the frontmost app whose title contains this text (case-insensitive) instead of the focused window." },
                    "root":     { "type": "string",  "description": "Expand this container ref (@<snapshot_id>:eN preferred, or @eN) from an existing snapshot instead of capturing fresh." },
                    "snapshot": { "type": "string",  "description": "Snapshot id to drill into with 'root' (default: latest; implied by a qualified root ref)." },
                    "depth":    { "type": "integer", "description": "Skeleton render depth (default: 3, max: 10). Higher = more nodes, more tokens." }
                }
            })),

        tool_rich("ghost_journal_marker",
            "Add an explicit milestone to Shadow's local work journal. Use after completing a meaningful desktop action so the timeline can explain what happened.",
            json!({
                "type": "object",
                "properties": {
                    "title":    { "type": "string", "description": "Short milestone title." },
                    "body":     { "type": "string", "description": "Optional one-sentence detail." },
                    "category": { "type": "string", "description": "Optional category such as Milestone, Deep Work, Communication, or Blocker." },
                    "app":      { "type": "string", "description": "Optional app or workflow name to associate with this marker." }
                },
                "required": ["title"]
            })),
    ]
}
