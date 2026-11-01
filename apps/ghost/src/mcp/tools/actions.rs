//! Action tool definitions (click, type, press, hotkey, scroll, hover, long press, drag, focus, window).

use serde_json::{json, Value};

use super::tool_rich;

pub(super) fn definitions() -> Vec<Value> {
    vec![
        // Actions (10)
        tool_rich("ghost_click",
            "Click or act on an element. With a ref (or a query and the default press), Ghost first acts semantically through accessibility (press, focus, toggle, check, expand, select, ...) without moving the mouse; it falls back to a mouse click only when allow_hid is true and the session has a desktop, and then reports via:\"hid\" with a fallback_reason. Results carry a disposition (delivery, retry) and, for stateful actions, a verification. Refs fail with STALE_REF when the element is gone or changed (re-snapshot) and AMBIGUOUS_TARGET when several elements match. When Ryu Island is running, shows a click-through ghost cursor with a short target label.",
            json!({
                "type": "object",
                "properties": {
                    "ref":       { "type": "string",  "description": "Target ref from a prior ghost_snapshot: @<snapshot_id>:eN (preferred) or @eN (latest snapshot). Re-identified live; errors STALE_REF if gone, AMBIGUOUS_TARGET if several elements match." },
                    "snapshot":  { "type": "string",  "description": "Snapshot id to resolve a bare @eN ref against (default: latest)." },
                    "action":    { "type": "string",  "enum": ["press", "focus", "toggle", "check", "uncheck", "expand", "collapse", "select", "scroll_into_view", "show_menu"], "description": "Semantic action for a ref or query target (default: press). check/uncheck/expand/collapse/select succeed without input when the element is already in that state." },
                    "value":     { "type": "string",  "description": "Value for an action that takes one. None of the actions above needs it; to set a field's text use ghost_type with replace:true." },
                    "allow_hid": { "type": "boolean", "description": "Allow a physical mouse fallback when the semantic action is unsupported (default: true). Physical input is always refused in a headless session." },
                    "query":     { "type": "string",  "description": "What to click (element text/name)." },
                    "dom_id":    { "type": "string",  "description": "Click by DOM id (CDP)." },
                    "dom_class": { "type": "string",  "description": "Click by CSS class (CDP)." },
                    "app":    { "type": "string",  "description": "Which app (auto-focuses if needed)." },
                    "x":      { "type": "number",  "description": "Click at X coordinate instead of element." },
                    "y":      { "type": "number",  "description": "Click at Y coordinate." },
                    "button": { "type": "string",  "description": "left (default), right, or middle." },
                    "count":  { "type": "integer", "description": "Click count: 1=single, 2=double, 3=triple." },
                    "mode":   { "type": "string",  "description": "auto (default): AX-press a resolved element without moving the physical cursor, else a courteous click that restores the cursor. hid: legacy warp-and-click. ax: AX-press only (errors if unpressable)." }
                }
            })),

        tool_rich("ghost_type",
            "Type text into a field. If 'into' or 'ref' is given, finds the field first. Returns typed_chars (a count; the text is never echoed). Read-back happens only for a ref with verify:true or replace:true, and those calls return a verification; other calls do not read the field back.",
            json!({
                "type": "object",
                "properties": {
                    "text":      { "type": "string",  "description": "Text to type." },
                    "ref":       { "type": "string",  "description": "Target field ref from a prior ghost_snapshot: @<snapshot_id>:eN (preferred) or @eN (latest snapshot). Errors STALE_REF if gone, AMBIGUOUS_TARGET if several elements match." },
                    "snapshot":  { "type": "string",  "description": "Snapshot id to resolve a bare @eN ref against (default: latest)." },
                    "replace":   { "type": "boolean", "description": "With a ref: set the field's whole value semantically (no keystrokes) and verify it (default: false)." },
                    "verify":    { "type": "boolean", "description": "With a ref: read the field back after typing and report a verification (default: false). Secure fields are never read back." },
                    "allow_hid": { "type": "boolean", "description": "Allow physical keystrokes (default: true). Physical input is always refused in a headless session." },
                    "into":      { "type": "string",  "description": "Target field name (finds via accessibility). If omitted, types at focus." },
                    "dom_id":    { "type": "string",  "description": "Target field by DOM id." },
                    "dom_class": { "type": "string",  "description": "Target field by CSS class." },
                    "app":       { "type": "string",  "description": "Which app." },
                    "clear":     { "type": "boolean", "description": "Clear field before typing (default: false)." }
                },
                "required": ["text"]
            })),

        tool_rich("ghost_press",
            "Press a single key. Always include app parameter to ensure correct target. System shortcuts that quit, lock, close or switch away (e.g. cmd+q, alt+f4) are refused with POLICY_DENIED unless force is set. Unknown keys or modifiers are INVALID_ARGS.",
            json!({
                "type": "object",
                "properties": {
                    "key":       { "type": "string",  "description": "Key name: return, tab, escape, space, delete, up, down, left, right, f1-f12." },
                    "modifiers": { "type": "array", "items": { "type": "string" }, "description": "Modifier keys: cmd, shift, option, control." },
                    "app":       { "type": "string",  "description": "Auto-focus this app first (IMPORTANT for synthetic input)." },
                    "force":     { "type": "boolean", "description": "Send a blocked system shortcut anyway. Set it only when the user explicitly asked for this shortcut (default: false)." }
                },
                "required": ["key"]
            })),

        tool_rich("ghost_hotkey",
            "Press a key combination. Modifier keys are auto-cleared afterward. Always include app parameter. System shortcuts that quit, lock, close or switch away (e.g. cmd+q, alt+f4) are refused with POLICY_DENIED unless force is set.",
            json!({
                "type": "object",
                "properties": {
                    "keys":  { "type": "array", "items": { "type": "string" }, "description": "Key combo, e.g. [\"cmd\", \"return\"] or [\"ctrl\", \"shift\", \"p\"]." },
                    "app":   { "type": "string", "description": "Auto-focus this app first (IMPORTANT for synthetic input)." },
                    "force": { "type": "boolean", "description": "Send a blocked system shortcut anyway. Set it only when the user explicitly asked for this shortcut (default: false)." }
                },
                "required": ["keys"]
            })),

        tool_rich("ghost_scroll",
            "Scroll content in a direction.",
            json!({
                "type": "object",
                "properties": {
                    "direction": { "type": "string",  "description": "up, down, left, or right." },
                    "amount":    { "type": "integer", "description": "Scroll amount in lines (default: 3)." },
                    "app":       { "type": "string",  "description": "Auto-focus this app first." },
                    "x":         { "type": "number",  "description": "Scroll at specific X position." },
                    "y":         { "type": "number",  "description": "Scroll at specific Y position." }
                },
                "required": ["direction"]
            })),

        tool_rich("ghost_hover",
            "Move cursor to an element or position WITHOUT clicking. Triggers tooltips, CSS :hover, menu navigation. Use ghost_read after to see what appeared.",
            json!({
                "type": "object",
                "properties": {
                    "query":     { "type": "string",  "description": "Element to hover over (centers cursor on element)." },
                    "dom_id":    { "type": "string",  "description": "Hover by DOM id (CDP)." },
                    "dom_class": { "type": "string",  "description": "Hover by CSS class." },
                    "app":       { "type": "string",  "description": "Which app (auto-focuses — hover effects need focus)." },
                    "x":         { "type": "number",  "description": "Hover at X coordinate instead of element." },
                    "y":         { "type": "number",  "description": "Hover at Y coordinate." }
                }
            })),

        tool_rich("ghost_long_press",
            "Press and hold at a position for a duration. Triggers long-press menus, context menus, and drag-initiation behaviors.",
            json!({
                "type": "object",
                "properties": {
                    "query":     { "type": "string",  "description": "Element to long-press (centers on element)." },
                    "dom_id":    { "type": "string",  "description": "Long-press by DOM id (CDP)." },
                    "dom_class": { "type": "string",  "description": "Long-press by CSS class." },
                    "app":       { "type": "string",  "description": "Which app (auto-focuses)." },
                    "x":         { "type": "number",  "description": "Long-press at X coordinate." },
                    "y":         { "type": "number",  "description": "Long-press at Y coordinate." },
                    "duration":  { "type": "number",  "description": "Hold duration in seconds (default: 1.0)." },
                    "button":    { "type": "string",  "description": "left (default) or right." }
                }
            })),

        tool_rich("ghost_drag",
            "Drag from one point to another. Use for: moving files, adjusting sliders, reordering lists, selecting text, resizing panes.",
            json!({
                "type": "object",
                "properties": {
                    "from_x":        { "type": "number", "description": "Start X coordinate." },
                    "from_y":        { "type": "number", "description": "Start Y coordinate." },
                    "to_x":          { "type": "number", "description": "End X coordinate." },
                    "to_y":          { "type": "number", "description": "End Y coordinate." },
                    "query":         { "type": "string", "description": "Element to drag (finds center as start point). Alternative to from_x/from_y." },
                    "dom_id":        { "type": "string", "description": "Find drag source by DOM id." },
                    "app":           { "type": "string", "description": "Which app (auto-focuses for synthetic input)." },
                    "duration":      { "type": "number", "description": "Drag duration in seconds (default: 0.5)." },
                    "hold_duration": { "type": "number", "description": "Seconds to hold at start before moving (default: 0.1)." }
                },
                "required": ["to_x", "to_y"]
            })),

        tool_rich("ghost_focus",
            "Bring an app or window to the front.",
            json!({
                "type": "object",
                "properties": {
                    "app":    { "type": "string", "description": "App name to focus." },
                    "window": { "type": "string", "description": "Window title substring to focus specific window." }
                },
                "required": ["app"]
            })),

        tool_rich("ghost_window",
            "Window management: minimize, maximize, close, restore, move, resize, or list windows.",
            json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "description": "minimize, maximize, close, restore, move, resize, or list." },
                    "app":    { "type": "string", "description": "Target app." },
                    "window": { "type": "string", "description": "Window title (if omitted, acts on frontmost window of app)." },
                    "x":      { "type": "number", "description": "X position for move." },
                    "y":      { "type": "number", "description": "Y position for move." },
                    "width":  { "type": "number", "description": "Width for resize." },
                    "height": { "type": "number", "description": "Height for resize." }
                },
                "required": ["action", "app"]
            })),
    ]
}
