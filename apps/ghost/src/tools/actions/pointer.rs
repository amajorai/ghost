//! Pointer tools: scroll, hover, long press and drag, plus the press helpers
//! `ghost_click` and `ghost_type` share. All of these are physical (HID) input, so
//! each tool refuses with POLICY_DENIED unless `allow_hid` (default true) holds and
//! the session has a desktop; the refusal comes before any app is focused.

use anyhow::Result;
use serde_json::{json, Value};

use ghost_eyes::{AXTree, GhostError, PlatformAXTree};
use ghost_hands::{
    execute_click, execute_drag, hover, long_press, mouse_click, scroll, ClickMode, ClickPlan,
    MouseButton,
};

use super::targets::{ax_target, cdp_target, center, dom_selectors, dom_target};
use super::{focus_requested_app, hid_disposition, require_hid};
use crate::tools::overlay_events;
use crate::tools::{f64_param, int_param, required_query, str_param};

/// Validate that a pointer call names a target (`dom_id`/`dom_class`, x/y, or a
/// non-blank query) before any physical-input check or focus.
fn ensure_locator(params: &Value, locators: &str) -> Result<()> {
    let has_dom = dom_selectors(params).next().is_some();
    let has_xy = params["x"].as_f64().is_some() && params["y"].as_f64().is_some();
    if has_dom || has_xy {
        return Ok(());
    }
    required_query(params, locators).map(|_| ())
}

pub async fn ghost_scroll(params: Value) -> Result<Value> {
    let direction = params["direction"]
        .as_str()
        .ok_or_else(|| GhostError::invalid_args("'direction' required"))?;
    let amount = int_param(&params, "amount", 3) as i32;
    let x = params["x"].as_f64().unwrap_or(960.0) as i32;
    let y = params["y"].as_f64().unwrap_or(540.0) as i32;
    require_hid(&params)?;

    focus_requested_app(str_param(&params, "app")).await;

    let dir = direction.to_string();
    overlay_events::emit("scroll", x, y, "ghost_scroll");
    tokio::task::spawn_blocking(move || scroll(x, y, &dir, amount)).await??;
    overlay_events::emit("done", x, y, "ghost_scroll");
    Ok(json!({ "success": true, "via": "hid", "disposition": hid_disposition() }))
}

pub async fn ghost_hover(params: Value) -> Result<Value> {
    ensure_locator(&params, "query, dom_id/dom_class, or x/y")?;
    require_hid(&params)?;
    focus_requested_app(str_param(&params, "app")).await;

    // dom_id / dom_class: CDP lookup by CSS #id or .class selector
    if let Some((el, _, method)) = dom_target(&params).await {
        let (sx, sy) = (el.x, el.y);
        tokio::task::spawn_blocking(move || hover(sx, sy)).await??;
        return Ok(
            json!({ "success": true, "x": sx, "y": sy, "method": method , "via": "hid", "disposition": hid_disposition() }),
        );
    }

    if let (Some(x), Some(y)) = (params["x"].as_f64(), params["y"].as_f64()) {
        tokio::task::spawn_blocking(move || hover(x as i32, y as i32)).await??;
        return Ok(
            json!({ "success": true, "x": x, "y": y , "via": "hid", "disposition": hid_disposition() }),
        );
    }

    let query = required_query(&params, "query, dom_id/dom_class, or x/y")?;
    if let Some((cx, cy, _)) = ax_target(query).await? {
        tokio::task::spawn_blocking(move || hover(cx, cy)).await??;
        return Ok(
            json!({ "success": true, "x": cx, "y": cy, "method": "ax_query" , "via": "hid", "disposition": hid_disposition() }),
        );
    }

    // CDP fallback for Chrome elements not in AX tree
    if let Some(el) = cdp_target(query).await {
        let (sx, sy) = (el.x, el.y);
        tokio::task::spawn_blocking(move || hover(sx, sy)).await??;
        return Ok(
            json!({ "success": true, "x": sx, "y": sy, "method": "cdp" , "via": "hid", "disposition": hid_disposition() }),
        );
    }

    Err(anyhow::anyhow!(
        "Could not find element '{}'. Specify x/y coordinates instead.",
        query
    ))
}

pub async fn ghost_long_press(params: Value) -> Result<Value> {
    let duration_secs = f64_param(&params, "duration", 1.0);
    let duration_ms = (duration_secs * 1000.0) as u64;
    let button = str_to_button(str_param(&params, "button").unwrap_or("left"));
    ensure_locator(&params, "x/y, dom_id/dom_class, or a query")?;
    require_hid(&params)?;

    focus_requested_app(str_param(&params, "app")).await;

    // dom_id / dom_class: CDP lookup by CSS #id or .class selector
    if let Some((el, _, method)) = dom_target(&params).await {
        let (sx, sy) = (el.x, el.y);
        tokio::task::spawn_blocking(move || long_press(sx, sy, duration_ms, button)).await??;
        return Ok(
            json!({ "success": true, "method": method, "via": "hid", "disposition": hid_disposition() }),
        );
    }

    if let (Some(x), Some(y)) = (params["x"].as_f64(), params["y"].as_f64()) {
        tokio::task::spawn_blocking(move || long_press(x as i32, y as i32, duration_ms, button))
            .await??;
        return Ok(json!({ "success": true, "via": "hid", "disposition": hid_disposition() }));
    }

    let query = required_query(&params, "x/y, dom_id/dom_class, or a query")?;
    if let Some((cx, cy, _)) = ax_target(query).await? {
        tokio::task::spawn_blocking(move || long_press(cx, cy, duration_ms, button)).await??;
        return Ok(json!({ "success": true, "via": "hid", "disposition": hid_disposition() }));
    }
    Err(anyhow::anyhow!(
        "Could not find element '{query}'. Specify x/y coordinates instead."
    ))
}

/// Resolve ghost_drag's `dom_id` source through Chrome DevTools (CDP) to a screen point,
/// as ghost_hover and ghost_long_press do. `None` when no dom_id was given, CDP is down,
/// or the element is not on the page; the caller then falls back to `query`.
async fn drag_source_from_dom_id(params: &Value) -> Option<(i32, i32)> {
    let id = str_param(params, "dom_id")?;
    let el = cdp_target(&format!("#{id}")).await?;
    Some((el.x, el.y))
}

pub async fn ghost_drag(params: Value) -> Result<Value> {
    let to_x = params["to_x"]
        .as_f64()
        .ok_or_else(|| GhostError::invalid_args("'to_x' required"))? as i32;
    let to_y = params["to_y"]
        .as_f64()
        .ok_or_else(|| GhostError::invalid_args("'to_y' required"))? as i32;
    let duration_secs = f64_param(&params, "duration", 0.5);
    let hold_secs = f64_param(&params, "hold_duration", 0.1);
    let duration_ms = (duration_secs * 1000.0) as u64;
    let hold_ms = (hold_secs * 1000.0) as u64;
    let has_from = params["from_x"].as_f64().is_some() && params["from_y"].as_f64().is_some();
    if !has_from && str_param(&params, "dom_id").is_none() {
        required_query(&params, "from_x/from_y, dom_id, or a query")?;
    }
    require_hid(&params)?;

    focus_requested_app(str_param(&params, "app")).await;

    // Drag has no accessibility equivalent, so any non-"hid" mode gets the courteous
    // restore: after the drop, the physical cursor returns to where it started if the
    // user did not grab the mouse mid-drag.
    let restore = !matches!(ClickMode::parse(str_param(&params, "mode")), ClickMode::Hid);

    let (from_x, from_y) =
        if let (Some(fx), Some(fy)) = (params["from_x"].as_f64(), params["from_y"].as_f64()) {
            (fx as i32, fy as i32)
        } else if let Some(point) = drag_source_from_dom_id(&params).await {
            point
        } else {
            let query = required_query(&params, "from_x/from_y, dom_id, or a query")?;
            let ax = PlatformAXTree::new()?;
            if let Some(el) = ax.find_element(query).await {
                if let Some(bounds) = &el.bounds {
                    center(bounds)
                } else {
                    return Err(anyhow::anyhow!(
                        "Element has no bounds. Specify from_x/from_y."
                    ));
                }
            } else {
                return Err(anyhow::anyhow!("Specify from_x/from_y or a query."));
            }
        };

    overlay_events::press_start(from_x, from_y, "ghost_drag");
    tokio::task::spawn_blocking(move || {
        execute_drag(from_x, from_y, to_x, to_y, duration_ms, hold_ms, restore)
    })
    .await??;
    overlay_events::press_end(to_x, to_y, "ghost_drag");
    Ok(json!({
        "success": true, "from": [from_x, from_y], "to": [to_x, to_y],
        "via": "hid", "disposition": hid_disposition(),
    }))
}

pub(super) fn str_to_button(s: &str) -> MouseButton {
    match s {
        "right" => MouseButton::Right,
        "middle" => MouseButton::Middle,
        _ => MouseButton::Left,
    }
}

/// Press at a resolved screen point with the call's `button` and `count`,
/// bracketed by the overlay's press events, and report how it was delivered.
pub(super) async fn click_at(
    params: &Value,
    x: i32,
    y: i32,
    plan: ClickPlan,
    target: Option<&str>,
) -> Result<&'static str> {
    let button = str_to_button(str_param(params, "button").unwrap_or("left"));
    let count = int_param(params, "count", 1) as u32;
    let intent = overlay_events::GhostIntent::click(target);
    overlay_events::press_start_with_intent(x, y, "ghost_click", &intent);
    let via =
        tokio::task::spawn_blocking(move || execute_click(x, y, button, count, plan)).await??;
    overlay_events::press_end_with_intent(x, y, "ghost_click", &intent);
    Ok(via)
}

/// Left-click a text field to focus it before typing.
pub(super) async fn left_click(x: i32, y: i32) -> Result<()> {
    tokio::task::spawn_blocking(move || mouse_click(x, y, MouseButton::Left, 1)).await?
}
