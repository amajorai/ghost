//! Wait, recipe, vision, annotate and learning tool definitions.

use serde_json::{json, Value};

use super::{tool, tool_rich};

pub(super) fn definitions() -> Vec<Value> {
    vec![
        // Wait (1)
        tool_rich("ghost_wait",
            "Wait for a condition instead of using fixed delays. Polls until condition is met or timeout.",
            json!({
                "type": "object",
                "properties": {
                    "condition": { "type": "string", "description": "urlContains, titleContains, elementExists, elementGone, urlChanged, titleChanged, delay. For delay, timeout is the sleep duration and no value is needed." },
                    "value":     { "type": "string", "description": "Match value (required for urlContains, titleContains, elementExists, elementGone; not used for delay)." },
                    "timeout":   { "type": "number", "minimum": 0, "maximum": 300, "description": "Max seconds to wait (default: 10, max: 300)." },
                    "interval":  { "type": "number", "minimum": 0, "description": "Poll interval in seconds (default: 0.5, min: 0.05)." }
                },
                "required": ["condition"]
            })),

        // Recipes (5)
        tool("ghost_recipes",
            "List all installed recipes with descriptions and parameters. ALWAYS check this first before doing multi-step tasks manually.",
            &[], &[]),

        tool_rich("ghost_run",
            "Execute a recipe with parameter substitution. Returns step-by-step results.",
            json!({
                "type": "object",
                "properties": {
                    "recipe": { "type": "string", "description": "Recipe name." },
                    "params": { "type": "object", "description": "Parameter values for substitution." }
                },
                "required": ["recipe"]
            })),

        tool_rich("ghost_recipe_show",
            "View full recipe details: steps, parameters, preconditions.",
            json!({
                "type": "object",
                "properties": { "name": { "type": "string", "description": "Recipe name." } },
                "required": ["name"]
            })),

        tool_rich("ghost_recipe_save",
            "Install a new recipe from JSON.",
            json!({
                "type": "object",
                "properties": { "recipe_json": { "type": "string", "description": "Complete recipe JSON string." } },
                "required": ["recipe_json"]
            })),

        tool_rich("ghost_recipe_delete",
            "Delete a recipe.",
            json!({
                "type": "object",
                "properties": { "name": { "type": "string", "description": "Recipe name to delete." } },
                "required": ["name"]
            })),

        // Vision (2)
        tool("ghost_parse_screen",
            "Detect ALL interactive UI elements on screen using vision (YOLO + VLM). Returns bounding boxes, types, and labels. Use when AX tree returns generic elements (web apps in Chrome).",
            &[
                ("app",            "string",  "Screenshot specific app window.", false),
                ("full_resolution","boolean", "Native resolution instead of 1280px resize (default: false).", false),
            ],
            &[]),

        tool_rich("ghost_ground",
            "Find precise screen coordinates for a described UI element using vision (VLM). Use when ghost_find can't locate the element or returns AXGroup elements.",
            json!({
                "type": "object",
                "properties": {
                    "description": { "type": "string", "description": "What to find (e.g. 'Compose button', 'Send button', 'search field')." },
                    "crop_box":    { "type": "array", "items": { "type": "number" }, "description": "Optional crop region [x1, y1, x2, y2] in logical points." }
                },
                "required": ["description"]
            })),

        // Annotate (1)
        tool_rich("ghost_annotate",
            "Screenshot with numbered labels [1], [2], [3]... on interactive UI elements. Returns an annotated image and a text index mapping each label to its element's role, name, and click coordinates. Call this for visual orientation, then use ghost_click with the x/y from the index. Zero ML — instant, uses the accessibility tree.",
            json!({
                "type": "object",
                "properties": {
                    "roles":      { "type": "array", "items": { "type": "string" }, "description": "AX roles to include (default: buttons, links, fields, checkboxes, combos, tabs, sliders)." },
                    "max_labels": { "type": "integer", "description": "Maximum number of labels (default: 50, max: 100)." }
                }
            })),

        // Learning (3)
        tool("ghost_learn_start",
            "Start observing the user's actions for workflow learning. Ghost records clicks, keystrokes, and app switches. Call ghost_learn_stop when done. Requires Input Monitoring permission (System Settings > Privacy & Security > Input Monitoring).",
            &[("task_description", "string", "Brief description of what the user is about to do (e.g., 'send an email in Gmail').", false)],
            &[]),

        tool("ghost_learn_stop",
            "Stop observing and return the recorded action sequence. Returns an array of observed actions with AX context for each click and typed text.",
            &[], &[]),

        tool("ghost_learn_status",
            "Check if learning mode is active, how many actions have been recorded, and how long the session has been running.",
            &[], &[]),
    ]
}
