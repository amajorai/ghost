//! Param-validation tests for the action tools (hermetic: no app focus, no AX).

use serde_json::json;

use ghost_hands::MouseButton;

use super::pointer::str_to_button;
use super::*;

mod contract_test;

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
