//! Window tools: focus an app or window, and window management actions.

use anyhow::Result;
use serde_json::{json, Value};

use ghost_eyes::GhostError;
use ghost_hands::{focus_app, window_action, WindowAction};

use crate::tools::str_param;

pub async fn ghost_focus(params: Value) -> Result<Value> {
    let app = params["app"]
        .as_str()
        .ok_or_else(|| GhostError::invalid_args("'app' required"))?;
    let window = str_param(&params, "window");
    // If a specific window title is given, try to focus it by exact title first
    if let Some(title) = window {
        if focus_app(title) {
            return Ok(json!({ "success": true, "method": "window_title", "window": title }));
        }
    }
    let success = focus_app(app);
    Ok(json!({ "success": success, "app": app }))
}

pub async fn ghost_window(params: Value) -> Result<Value> {
    let action_str = params["action"]
        .as_str()
        .ok_or_else(|| GhostError::invalid_args("'action' required"))?;
    let app = params["app"]
        .as_str()
        .ok_or_else(|| GhostError::invalid_args("'app' required"))?;
    let window_title = str_param(&params, "window");

    let action = match action_str {
        "minimize" => WindowAction::Minimize,
        "maximize" => WindowAction::Maximize,
        "close" => WindowAction::Close,
        "restore" => WindowAction::Restore,
        "move" => WindowAction::Move {
            x: params["x"].as_f64().unwrap_or(0.0) as i32,
            y: params["y"].as_f64().unwrap_or(0.0) as i32,
        },
        "resize" => WindowAction::Resize {
            width: params["width"].as_f64().unwrap_or(800.0) as u32,
            height: params["height"].as_f64().unwrap_or(600.0) as u32,
        },
        "list" => WindowAction::List,
        _ => {
            return Err(GhostError::invalid_args(format!(
                "Unknown action '{action_str}'. Use: minimize, maximize, close, restore, move, resize, list."
            ))
            .into())
        }
    };

    let app_owned = app.to_string();
    let title_owned = window_title.map(|s| s.to_string());
    tokio::task::spawn_blocking(move || window_action(&action, &app_owned, title_owned.as_deref()))
        .await?
}
