use anyhow::Result;
use serde_json::{json, Value};

use ghost_eyes::{AXTree, Bounds, PlatformAXTree};
use ghost_hands::{
    execute_click, execute_drag, focus_app, hover, long_press, mouse_click, plan_click, press_key,
    scroll, send_hotkey, type_text, window_action, ClickMode, ClickPlan, MouseButton, WindowAction,
};

use super::overlay_events;
use super::{bool_param, f64_param, int_param, nonblank_str_param, required_query, str_param};

pub async fn ghost_click(params: Value) -> Result<Value> {
    focus_requested_app(str_param(&params, "app")).await;

    // How to click: "auto" (default) presses via the accessibility API when the
    // target is a real AX element (no cursor hijack), else a courteous HID click;
    // "hid" is the legacy warp-and-click; "ax" is AX-press-only. See ghost_hands::mode.
    let mode = ClickMode::parse(str_param(&params, "mode"));

    // Ref from a prior ghost_snapshot — re-identify (STALE_REF if gone) then click.
    if let Some(r) = str_param(&params, "ref") {
        let (x, y) = crate::tools::snapshot::resolve_ref(r).await?;
        let via = click_at(&params, x, y, plan_click(mode, false, false), None).await?;
        return Ok(
            json!({ "success": true, "x": x, "y": y, "ref": r, "method": "ref", "via": via }),
        );
    }

    // If x/y given, use coordinates directly
    if let (Some(x), Some(y)) = (params["x"].as_f64(), params["y"].as_f64()) {
        let plan = plan_click(mode, false, true);
        let via = click_at(&params, x as i32, y as i32, plan, None).await?;
        return Ok(json!({ "success": true, "x": x, "y": y, "method": "coordinates", "via": via }));
    }

    // dom_id / dom_class: direct CDP lookup by CSS #id or .class selector
    if let Some((el, selector, method)) = dom_target(&params).await {
        let plan = plan_click(mode, false, false);
        let via = click_at(&params, el.x, el.y, plan, Some(&selector)).await?;
        return Ok(
            json!({ "success": true, "x": el.x, "y": el.y, "element": el.text, "method": method, "via": via }),
        );
    }

    // Find element by query
    let query = required_query(&params, "ref, query, dom_id/dom_class, or x/y")?;
    if let Some((cx, cy, title)) = ax_target(query).await? {
        // A real AX element was matched → auto/ax modes press it directly (no
        // cursor warp); the physical-click fallback keeps the cursor courteous.
        let plan = plan_click(mode, true, false);
        let via = click_at(&params, cx, cy, plan, Some(query)).await?;
        return Ok(
            json!({ "success": true, "x": cx, "y": cy, "element": title, "method": "ax_query", "via": via }),
        );
    }

    // CDP fallback: Chrome elements not exposed in the AX tree (iframes, SPAs, Gmail, etc.)
    if let Some(el) = cdp_target(query).await {
        let plan = plan_click(mode, false, false);
        let via = click_at(&params, el.x, el.y, plan, Some(query)).await?;
        return Ok(
            json!({ "success": true, "x": el.x, "y": el.y, "element": el.text, "method": "cdp", "via": via }),
        );
    }

    Err(anyhow::anyhow!("Could not find element '{}'. Try ghost_annotate to see numbered labels, or specify x/y coordinates.", query))
}

pub async fn ghost_type(params: Value) -> Result<Value> {
    let text = params["text"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("'text' required"))?;
    // A blank `into` would match the root AX node and click the window centre.
    let into = nonblank_str_param(&params, "into");
    let clear = bool_param(&params, "clear", false);

    focus_requested_app(str_param(&params, "app")).await;

    // Ref from a prior ghost_snapshot — click to focus the field, then type.
    if let Some(r) = str_param(&params, "ref") {
        let (x, y) = crate::tools::snapshot::resolve_ref(r).await?;
        left_click(x, y).await?;
        settle().await;
        type_with_overlay(text, clear, x, y).await?;
        return Ok(json!({ "success": true, "typed": text, "ref": r, "method": "ref" }));
    }

    // dom_id, then dom_class: click each field found via CDP, then fall through to type
    for (selector, _) in dom_selectors(&params) {
        if let Some(el) = cdp_target(&selector).await {
            left_click(el.x, el.y).await?;
            settle().await;
        }
    }

    // If 'into' is given, click the target field first (AX → CDP fallback)
    if let Some(target) = into {
        if let Some((cx, cy, _)) = ax_target(target).await? {
            left_click(cx, cy).await?;
        } else if let Some(el) = cdp_target(target).await {
            left_click(el.x, el.y).await?;
        }
        settle().await;
    }

    type_with_overlay(text, clear, 0, 0).await?;
    Ok(json!({ "success": true, "typed": text }))
}

pub async fn ghost_press(params: Value) -> Result<Value> {
    let key = params["key"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("'key' required"))?;
    focus_requested_app(str_param(&params, "app")).await;

    let modifiers: Vec<String> = params["modifiers"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let key_owned = key.to_string();
    tokio::task::spawn_blocking(move || {
        let mod_refs: Vec<&str> = modifiers.iter().map(|s| s.as_str()).collect();
        press_key(&key_owned, &mod_refs)
    })
    .await??;
    Ok(json!({ "success": true, "key": key }))
}

pub async fn ghost_hotkey(params: Value) -> Result<Value> {
    let keys: Vec<String> = params["keys"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("'keys' required (array)"))?
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();

    focus_requested_app(str_param(&params, "app")).await;

    tokio::task::spawn_blocking(move || {
        let refs: Vec<&str> = keys.iter().map(|s| s.as_str()).collect();
        send_hotkey(&refs)
    })
    .await??;
    Ok(json!({ "success": true, "keys": params["keys"] }))
}

pub async fn ghost_scroll(params: Value) -> Result<Value> {
    let direction = params["direction"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("'direction' required"))?;
    let amount = int_param(&params, "amount", 3) as i32;
    let x = params["x"].as_f64().unwrap_or(960.0) as i32;
    let y = params["y"].as_f64().unwrap_or(540.0) as i32;

    focus_requested_app(str_param(&params, "app")).await;

    let dir = direction.to_string();
    overlay_events::emit("scroll", x, y, "ghost_scroll");
    tokio::task::spawn_blocking(move || scroll(x, y, &dir, amount)).await??;
    overlay_events::emit("done", x, y, "ghost_scroll");
    Ok(json!({ "success": true }))
}

pub async fn ghost_hover(params: Value) -> Result<Value> {
    focus_requested_app(str_param(&params, "app")).await;

    // dom_id / dom_class: CDP lookup by CSS #id or .class selector
    if let Some((el, _, method)) = dom_target(&params).await {
        let (sx, sy) = (el.x, el.y);
        tokio::task::spawn_blocking(move || hover(sx, sy)).await??;
        return Ok(json!({ "success": true, "x": sx, "y": sy, "method": method }));
    }

    if let (Some(x), Some(y)) = (params["x"].as_f64(), params["y"].as_f64()) {
        tokio::task::spawn_blocking(move || hover(x as i32, y as i32)).await??;
        return Ok(json!({ "success": true, "x": x, "y": y }));
    }

    let query = required_query(&params, "query, dom_id/dom_class, or x/y")?;
    if let Some((cx, cy, _)) = ax_target(query).await? {
        tokio::task::spawn_blocking(move || hover(cx, cy)).await??;
        return Ok(json!({ "success": true, "x": cx, "y": cy, "method": "ax_query" }));
    }

    // CDP fallback for Chrome elements not in AX tree
    if let Some(el) = cdp_target(query).await {
        let (sx, sy) = (el.x, el.y);
        tokio::task::spawn_blocking(move || hover(sx, sy)).await??;
        return Ok(json!({ "success": true, "x": sx, "y": sy, "method": "cdp" }));
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

    focus_requested_app(str_param(&params, "app")).await;

    // dom_id / dom_class: CDP lookup by CSS #id or .class selector
    if let Some((el, _, method)) = dom_target(&params).await {
        let (sx, sy) = (el.x, el.y);
        tokio::task::spawn_blocking(move || long_press(sx, sy, duration_ms, button)).await??;
        return Ok(json!({ "success": true, "method": method }));
    }

    if let (Some(x), Some(y)) = (params["x"].as_f64(), params["y"].as_f64()) {
        tokio::task::spawn_blocking(move || long_press(x as i32, y as i32, duration_ms, button))
            .await??;
        return Ok(json!({ "success": true }));
    }

    let query = required_query(&params, "x/y, dom_id/dom_class, or a query")?;
    if let Some((cx, cy, _)) = ax_target(query).await? {
        tokio::task::spawn_blocking(move || long_press(cx, cy, duration_ms, button)).await??;
        return Ok(json!({ "success": true }));
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
        .ok_or_else(|| anyhow::anyhow!("'to_x' required"))? as i32;
    let to_y = params["to_y"]
        .as_f64()
        .ok_or_else(|| anyhow::anyhow!("'to_y' required"))? as i32;
    let duration_secs = f64_param(&params, "duration", 0.5);
    let hold_secs = f64_param(&params, "hold_duration", 0.1);
    let duration_ms = (duration_secs * 1000.0) as u64;
    let hold_ms = (hold_secs * 1000.0) as u64;

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
    Ok(json!({ "success": true, "from": [from_x, from_y], "to": [to_x, to_y] }))
}

pub async fn ghost_focus(params: Value) -> Result<Value> {
    let app = params["app"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("'app' required"))?;
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
        .ok_or_else(|| anyhow::anyhow!("'action' required"))?;
    let app = params["app"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("'app' required"))?;
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
            return Err(anyhow::anyhow!(
                "Unknown action '{}'. Use: minimize, maximize, close, restore, move, resize, list.",
                action_str
            ))
        }
    };

    let app_owned = app.to_string();
    let title_owned = window_title.map(|s| s.to_string());
    tokio::task::spawn_blocking(move || window_action(&action, &app_owned, title_owned.as_deref()))
        .await?
}

fn str_to_button(s: &str) -> MouseButton {
    match s {
        "right" => MouseButton::Right,
        "middle" => MouseButton::Middle,
        _ => MouseButton::Left,
    }
}

/// Keep the overlay label useful without putting the full query or selector in
/// the action protocol. The overlay applies a final length bound as well.
fn click_intent(target: Option<&str>) -> String {
    match target.map(str::trim).filter(|value| !value.is_empty()) {
        Some(target) => format!("Click “{target}”"),
        None => "Click".to_owned(),
    }
}

/// Bring the requested app forward and give its window a moment to settle.
async fn focus_requested_app(app: Option<&str>) {
    if let Some(app_name) = app {
        focus_app(app_name);
        settle().await;
    }
}

/// The short pause that lets focus or a click land before the next input.
async fn settle() {
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
}

/// Press at a resolved screen point with the call's `button` and `count`,
/// bracketed by the overlay's press events, and report how it was delivered.
async fn click_at(
    params: &Value,
    x: i32,
    y: i32,
    plan: ClickPlan,
    target: Option<&str>,
) -> Result<&'static str> {
    let button = str_to_button(str_param(params, "button").unwrap_or("left"));
    let count = int_param(params, "count", 1) as u32;
    let intent = click_intent(target);
    overlay_events::press_start_with_intent(x, y, "ghost_click", &intent);
    let via =
        tokio::task::spawn_blocking(move || execute_click(x, y, button, count, plan)).await??;
    overlay_events::press_end_with_intent(x, y, "ghost_click", &intent);
    Ok(via)
}

/// Left-click a text field to focus it before typing.
async fn left_click(x: i32, y: i32) -> Result<()> {
    tokio::task::spawn_blocking(move || mouse_click(x, y, MouseButton::Left, 1)).await?
}

/// Type `text`, bracketed by the overlay's typing events at `(x, y)`.
async fn type_with_overlay(text: &str, clear: bool, x: i32, y: i32) -> Result<()> {
    let text_owned = text.to_string();
    overlay_events::emit("type", x, y, "ghost_type");
    tokio::task::spawn_blocking(move || type_text(&text_owned, clear)).await??;
    overlay_events::emit("done", x, y, "ghost_type");
    Ok(())
}

/// Screen-space center of an accessibility element's bounds.
fn center(bounds: &Bounds) -> (i32, i32) {
    (
        bounds.x + bounds.width as i32 / 2,
        bounds.y + bounds.height as i32 / 2,
    )
}

/// Center and title of the accessibility element matching `query`, or `None`
/// when nothing matches or the match has no bounds.
async fn ax_target(query: &str) -> Result<Option<(i32, i32, Option<String>)>> {
    let ax = PlatformAXTree::new()?;
    Ok(ax.find_element(query).await.and_then(|el| {
        let (x, y) = center(el.bounds.as_ref()?);
        Some((x, y, el.title))
    }))
}

/// A Chrome element resolved through CDP to screen-absolute coordinates.
struct CdpTarget {
    x: i32,
    y: i32,
    text: String,
}

/// First Chrome element matching a CSS selector or query, or `None` when CDP is
/// unavailable or nothing matches.
async fn cdp_target(query: &str) -> Option<CdpTarget> {
    if !ghost_core::cdp::is_available().await {
        return None;
    }
    let el = ghost_core::cdp::find_elements(query)
        .await
        .ok()?
        .into_iter()
        .next()?;
    let (win_x, win_y) = chrome_window_origin();
    let (x, y) = ghost_core::cdp::viewport_to_screen(el.center_x, el.center_y, win_x, win_y);
    Some(CdpTarget {
        x,
        y,
        text: el.text,
    })
}

/// The `dom_id` (`#id`) then `dom_class` (`.class`) CSS selectors a call names,
/// each with the `method` a match reports.
fn dom_selectors(params: &Value) -> impl Iterator<Item = (String, &'static str)> + '_ {
    [
        ("dom_id", "#", "cdp_dom_id"),
        ("dom_class", ".", "cdp_dom_class"),
    ]
    .into_iter()
    .filter_map(move |(key, prefix, method)| {
        str_param(params, key).map(|value| (format!("{prefix}{value}"), method))
    })
}

/// The first `dom_selectors` element Chrome resolves, with its selector and method.
async fn dom_target(params: &Value) -> Option<(CdpTarget, String, &'static str)> {
    for (selector, method) in dom_selectors(params) {
        if let Some(el) = cdp_target(&selector).await {
            return Some((el, selector, method));
        }
    }
    None
}

/// Return the screen-space top-left origin of the Chrome window.
/// Used to convert CDP viewport-relative coordinates to screen-absolute.
fn chrome_window_origin() -> (i32, i32) {
    #[cfg(target_os = "windows")]
    {
        use windows::core::w;
        use windows::Win32::Foundation::RECT;
        use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowRect};
        unsafe {
            if let Ok(hwnd) = FindWindowW(w!("Chrome_WidgetWin_1"), windows::core::PCWSTR::null()) {
                if !hwnd.0.is_null() {
                    let mut r = RECT::default();
                    let _ = GetWindowRect(hwnd, &mut r);
                    return (r.left, r.top);
                }
            }
        }
    }
    (0, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn str_to_button_maps_known_names_and_defaults_to_left() {
        assert!(matches!(str_to_button("right"), MouseButton::Right));
        assert!(matches!(str_to_button("middle"), MouseButton::Middle));
        assert!(matches!(str_to_button("left"), MouseButton::Left));
        // Anything unrecognized falls back to left.
        assert!(matches!(str_to_button("garbage"), MouseButton::Left));
        assert!(matches!(str_to_button(""), MouseButton::Left));
    }

    // The action handlers validate required params BEFORE touching the accessibility
    // tree or focusing an app, so these error paths are hermetic (no `app` passed, so
    // no `focus_app` OS call is reached).

    #[tokio::test]
    async fn ghost_type_requires_text() {
        let err = ghost_type(json!({})).await.expect_err("text is required");
        assert!(err.to_string().contains("'text' required"));
    }

    #[tokio::test]
    async fn ghost_press_requires_key() {
        let err = ghost_press(json!({})).await.expect_err("key is required");
        assert!(err.to_string().contains("'key' required"));
    }

    #[tokio::test]
    async fn ghost_hotkey_requires_keys_array() {
        let err = ghost_hotkey(json!({})).await.expect_err("keys is required");
        assert!(err.to_string().contains("'keys' required"));
    }

    #[tokio::test]
    async fn ghost_scroll_requires_direction() {
        let err = ghost_scroll(json!({}))
            .await
            .expect_err("direction is required");
        assert!(err.to_string().contains("'direction' required"));
    }

    #[tokio::test]
    async fn ghost_drag_requires_to_coordinates() {
        let err = ghost_drag(json!({})).await.expect_err("to_x is required");
        assert!(err.to_string().contains("'to_x' required"));
        let err = ghost_drag(json!({ "to_x": 5.0 }))
            .await
            .expect_err("to_y is required");
        assert!(err.to_string().contains("'to_y' required"));
    }

    // A missing or blank query must fail rather than match the root AX node and act
    // on the centre of the focused window. These return before any AX/CDP lookup.
    #[tokio::test]
    async fn element_tools_reject_missing_or_blank_query() {
        for params in [
            json!({}),
            json!({ "query": "" }),
            json!({ "query": "  " }),
            json!({ "x": 5.0 }),
        ] {
            let err = ghost_click(params.clone())
                .await
                .expect_err("click needs a locator");
            assert!(err.to_string().contains("Specify"), "{err}");
            let err = ghost_hover(params.clone())
                .await
                .expect_err("hover needs a locator");
            assert!(err.to_string().contains("Specify"), "{err}");
            let err = ghost_long_press(params)
                .await
                .expect_err("long press needs a locator");
            assert!(err.to_string().contains("Specify"), "{err}");
        }
        let err = ghost_drag(json!({ "to_x": 1.0, "to_y": 2.0, "query": "" }))
            .await
            .expect_err("drag needs a source");
        assert!(err.to_string().contains("from_x/from_y"), "{err}");
    }

    #[tokio::test]
    async fn ghost_focus_requires_app() {
        let err = ghost_focus(json!({})).await.expect_err("app is required");
        assert!(err.to_string().contains("'app' required"));
    }

    #[tokio::test]
    async fn ghost_window_validates_action_then_app_then_unknown_action() {
        let err = ghost_window(json!({})).await.expect_err("action required");
        assert!(err.to_string().contains("'action' required"));

        let err = ghost_window(json!({ "action": "minimize" }))
            .await
            .expect_err("app required");
        assert!(err.to_string().contains("'app' required"));

        // A syntactically valid call with an unknown action errors before any OS call.
        let err = ghost_window(json!({ "action": "teleport", "app": "Finder" }))
            .await
            .expect_err("unknown action");
        assert!(err.to_string().contains("Unknown action"));
    }
}
