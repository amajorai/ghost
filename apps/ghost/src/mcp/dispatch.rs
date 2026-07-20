// Tool name → handler routing.

use anyhow::Result;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::tools::{
    actions, annotate, journal, learning, perception, recipes, snapshot, vision, wait,
};

/// Process-wide serialization of synthetic input. Pointer/keyboard tools warp the
/// cursor and post OS input events; two of them interleaving (e.g. a background
/// agent turn racing another) would produce a garbled event stream and, with the
/// cursor-restore courtesy, fight over the physical cursor. Perception, focus, and
/// window/recipe tools are read-only or coarse and need not serialize, so they are
/// intentionally left out. Cross-process serialization (two `ghost` processes) is a
/// documented followup — this guards a single process only.
static ACTION_LOCK: Mutex<()> = Mutex::const_new(());

/// Whether `tool_name` synthesises pointer/keyboard input and must run under
/// [`ACTION_LOCK`].
fn is_input_action(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "ghost_click"
            | "ghost_type"
            | "ghost_press"
            | "ghost_hotkey"
            | "ghost_scroll"
            | "ghost_hover"
            | "ghost_long_press"
            | "ghost_drag"
    )
}

pub async fn dispatch(tool_name: &str, params: Value) -> Result<Value> {
    if is_input_action(tool_name) {
        let _guard = ACTION_LOCK.lock().await;
        return dispatch_inner(tool_name, params).await;
    }
    dispatch_inner(tool_name, params).await
}

async fn dispatch_inner(tool_name: &str, params: Value) -> Result<Value> {
    match tool_name {
        // Perception
        "ghost_context" => perception::ghost_context(params).await,
        "ghost_state" => perception::ghost_state(params).await,
        "ghost_find" => perception::ghost_find(params).await,
        "ghost_read" => perception::ghost_read(params).await,
        "ghost_inspect" => perception::ghost_inspect(params).await,
        "ghost_element_at" => perception::ghost_element_at(params).await,
        "ghost_screenshot" => perception::ghost_screenshot(params).await,
        "ghost_snapshot" => snapshot::ghost_snapshot(params).await,
        "ghost_journal_marker" => journal::ghost_journal_marker(params).await,

        // Actions
        "ghost_click" => actions::ghost_click(params).await,
        "ghost_type" => actions::ghost_type(params).await,
        "ghost_press" => actions::ghost_press(params).await,
        "ghost_hotkey" => actions::ghost_hotkey(params).await,
        "ghost_scroll" => actions::ghost_scroll(params).await,
        "ghost_hover" => actions::ghost_hover(params).await,
        "ghost_long_press" => actions::ghost_long_press(params).await,
        "ghost_drag" => actions::ghost_drag(params).await,
        "ghost_focus" => actions::ghost_focus(params).await,
        "ghost_window" => actions::ghost_window(params).await,

        // Wait
        "ghost_wait" => wait::ghost_wait(params).await,

        // Recipes
        "ghost_recipes" => recipes::ghost_recipes(params).await,
        "ghost_run" => recipes::ghost_run(params).await,
        "ghost_recipe_show" => recipes::ghost_recipe_show(params).await,
        "ghost_recipe_save" => recipes::ghost_recipe_save(params).await,
        "ghost_recipe_delete" => recipes::ghost_recipe_delete(params).await,

        // Vision
        "ghost_ground" => vision::ghost_ground(params).await,
        "ghost_parse_screen" => vision::ghost_parse_screen(params).await,

        // Annotate
        "ghost_annotate" => annotate::ghost_annotate(params).await,

        // Learning
        "ghost_learn_start" => learning::ghost_learn_start(params).await,
        "ghost_learn_stop" => learning::ghost_learn_stop(params).await,
        "ghost_learn_status" => learning::ghost_learn_status(params).await,

        _ => Err(anyhow::anyhow!("Unknown tool: {tool_name}")),
    }
}
