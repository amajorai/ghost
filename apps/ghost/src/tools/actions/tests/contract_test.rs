//! Contract tests for the action tools: typed argument errors, semantic-action
//! wiring, query matching, physical-input gating, typing redaction and the hotkey
//! `force` param. Hermetic: every call here fails before any input, focus or AX walk.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use ghost_eyes::{AXTreeNode, Bounds, Delivery, ElementAction, ErrorCode, GhostError};
use ghost_hands::{ClickMode, ClickPlan, KeyError};

use super::super::click::{cdp_click_allowed, hid_fallback_plan, parse_action, pick_query_match};
use super::super::keys::key_options;
use super::super::semantic::allows_hid_fallback;
use super::super::typing::{redact_typing_result, typed_mismatch};
use super::super::*;

/// The contract code an action error carries (GhostError or hotkey KeyError).
fn code_of(err: &anyhow::Error) -> String {
    if let Some(g) = err.downcast_ref::<GhostError>() {
        return g.code.as_str().to_string();
    }
    if let Some(k) = err.downcast_ref::<KeyError>() {
        return k.code().to_string();
    }
    panic!("untyped action error: {err}");
}

fn ghost(err: &anyhow::Error) -> &GhostError {
    err.downcast_ref::<GhostError>()
        .unwrap_or_else(|| panic!("expected a GhostError, got: {err}"))
}

fn node(role: &str, title: &str, x: i32) -> AXTreeNode {
    AXTreeNode {
        role: role.to_string(),
        title: Some(title.to_string()),
        bounds: Some(Bounds {
            x,
            y: 0,
            width: 10,
            height: 10,
        }),
        ..AXTreeNode::default()
    }
}

fn window(children: Vec<AXTreeNode>) -> AXTreeNode {
    AXTreeNode {
        role: "AXWindow".to_string(),
        title: Some("Main".to_string()),
        children,
        ..AXTreeNode::default()
    }
}

/// A blocked combo on this OS, as ghost_hotkey keys.
fn blocked_keys() -> Value {
    if cfg!(target_os = "macos") {
        json!(["cmd", "q"])
    } else {
        json!(["alt", "f4"])
    }
}

#[tokio::test]
async fn validators_return_invalid_args() {
    let cases: Vec<anyhow::Error> = vec![
        ghost_type(json!({})).await.unwrap_err(),
        ghost_type(json!({ "text": "x", "replace": true }))
            .await
            .unwrap_err(),
        ghost_press(json!({})).await.unwrap_err(),
        ghost_hotkey(json!({})).await.unwrap_err(),
        ghost_scroll(json!({})).await.unwrap_err(),
        ghost_drag(json!({})).await.unwrap_err(),
        ghost_focus(json!({})).await.unwrap_err(),
        ghost_window(json!({ "action": "teleport", "app": "Finder" }))
            .await
            .unwrap_err(),
        ghost_click(json!({ "ref": "@e1", "action": "explode" }))
            .await
            .unwrap_err(),
        ghost_click(json!({ "x": 1.0, "y": 2.0, "action": "toggle" }))
            .await
            .unwrap_err(),
        ghost_click(json!({ "query": "OK", "action": "check", "mode": "hid" }))
            .await
            .unwrap_err(),
        ghost_click(json!({ "ref": "@bad:x" })).await.unwrap_err(),
        ghost_press(json!({ "key": "notakey" })).await.unwrap_err(),
    ];
    for err in &cases {
        assert_eq!(code_of(err), "INVALID_ARGS", "{err}");
    }
}

#[test]
fn click_action_param_parses_through_element_action() {
    assert_eq!(parse_action(&json!({})).unwrap(), ElementAction::Press);
    assert_eq!(
        parse_action(&json!({ "action": "toggle" })).unwrap(),
        ElementAction::Toggle
    );
    assert_eq!(
        parse_action(&json!({ "action": " scroll_into_view " })).unwrap(),
        ElementAction::ScrollIntoView
    );
    for bad in [
        json!({ "action": "explode" }),
        json!({ "action": "set_value" }),
    ] {
        let err = parse_action(&bad).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgs);
    }
}

#[test]
fn query_with_ambiguous_non_press_action_is_ambiguous_target() {
    let tree = window(vec![
        node("AXCheckBox", "Remember me", 0),
        node("AXCheckBox", "Remember me", 50),
    ]);
    let err = pick_query_match(&tree, "remember", &ElementAction::Check)
        .err()
        .expect("two matches cannot be checked");
    assert_eq!(err.code, ErrorCode::AmbiguousTarget);
    assert_eq!(err.delivery, Delivery::NotDelivered);
    let details = err.details.expect("candidates");
    assert_eq!(details["candidate_count"], 2);
    assert_eq!(details["candidates"][1]["path"], json!([1]));

    // press keeps the legacy first-match choice and reports the count.
    let m = pick_query_match(&tree, "remember", &ElementAction::Press)
        .unwrap()
        .expect("a match");
    assert_eq!(m.path, vec![0]);
    assert_eq!(m.match_count, 2);

    // A unique match is fine for any action.
    let unique = window(vec![
        node("AXCheckBox", "Remember me", 0),
        node("AXButton", "OK", 50),
    ]);
    let m = pick_query_match(&unique, "remember", &ElementAction::Check)
        .unwrap()
        .expect("a match");
    assert_eq!(m.path, vec![0]);
    assert!(
        pick_query_match(&unique, "nothing here", &ElementAction::Check)
            .unwrap()
            .is_none()
    );
}

#[test]
fn query_never_matches_a_secure_value() {
    let mut secret = node("AXTextField", "Password", 0);
    secret.secure = true;
    secret.value = Some("hunter2".to_string());
    let tree = window(vec![secret]);
    assert!(pick_query_match(&tree, "hunter2", &ElementAction::Press)
        .unwrap()
        .is_none());
}

#[test]
fn element_hid_fallback_never_plans_an_ax_press() {
    // ax_press_at runs only for AxOnly/AxThenHidRestore plans; element targets
    // (refs and queries) never use them.
    for mode in [ClickMode::Auto, ClickMode::Ax, ClickMode::Hid] {
        let plan = hid_fallback_plan(mode);
        assert!(
            matches!(plan, ClickPlan::HidRestore | ClickPlan::HidPlain),
            "{mode:?} => {plan:?}"
        );
    }
}

#[test]
fn ax_mode_never_clicks_a_cdp_match() {
    let err = cdp_click_allowed(ClickMode::Ax).unwrap_err();
    assert_eq!(err.code, ghost_eyes::ErrorCode::ActionNotSupported);
    assert!(cdp_click_allowed(ClickMode::Auto).is_ok());
    assert!(cdp_click_allowed(ClickMode::Hid).is_ok());
}

#[test]
fn hid_fallback_only_after_undelivered_unsupported() {
    assert!(allows_hid_fallback(&GhostError::not_supported(
        "no AXPress"
    )));
    assert!(allows_hid_fallback(&GhostError::new(
        ErrorCode::PlatformNotSupported,
        "no backend"
    )));
    let maybe_sent =
        GhostError::not_supported("partial").with_delivery(Delivery::DeliveryUncertain);
    assert!(!allows_hid_fallback(&maybe_sent));
    assert!(!allows_hid_fallback(&GhostError::new(
        ErrorCode::ActionFailed,
        "kAXErrorFailure"
    )));
    assert!(!allows_hid_fallback(&GhostError::stale_ref("gone")));
}

#[tokio::test]
async fn allow_hid_false_is_policy_denied_before_any_input() {
    let calls: Vec<anyhow::Error> = vec![
        ghost_scroll(json!({ "direction": "down", "allow_hid": false }))
            .await
            .unwrap_err(),
        ghost_hover(json!({ "x": 1.0, "y": 2.0, "allow_hid": false }))
            .await
            .unwrap_err(),
        ghost_long_press(json!({ "x": 1.0, "y": 2.0, "allow_hid": false }))
            .await
            .unwrap_err(),
        ghost_drag(
            json!({ "from_x": 1.0, "from_y": 1.0, "to_x": 5.0, "to_y": 5.0, "allow_hid": false }),
        )
        .await
        .unwrap_err(),
        ghost_press(json!({ "key": "enter", "allow_hid": false }))
            .await
            .unwrap_err(),
        ghost_hotkey(json!({ "keys": ["ctrl", "c"], "allow_hid": false }))
            .await
            .unwrap_err(),
        ghost_type(json!({ "text": "hello", "allow_hid": false }))
            .await
            .unwrap_err(),
        // A ref without replace still types physically: refused before resolution.
        ghost_type(json!({ "text": "hello", "ref": "@e1", "allow_hid": false }))
            .await
            .unwrap_err(),
        ghost_click(json!({ "x": 1.0, "y": 2.0, "mode": "hid", "allow_hid": false }))
            .await
            .unwrap_err(),
        ghost_click(json!({ "ref": "@e1", "mode": "hid", "allow_hid": false }))
            .await
            .unwrap_err(),
    ];
    for err in &calls {
        let g = ghost(err);
        assert_eq!(g.code, ErrorCode::PolicyDenied, "{err}");
        assert_eq!(g.delivery, Delivery::NotDelivered);
        assert!(
            g.suggestion
                .as_deref()
                .unwrap_or("")
                .contains("allow_hid:true"),
            "{err}"
        );
    }
}

#[test]
fn typing_errors_never_carry_the_typed_text() {
    let platform = anyhow::anyhow!("Cannot type 'hunter2' on this layout");
    let err = redact_typing_result(Err(platform), 7).unwrap_err();
    assert_eq!(err.code, ErrorCode::ActionFailed);
    assert_eq!(err.delivery, Delivery::Unknown);
    assert!(!err.to_string().contains("hunter2"), "{err}");
    assert!(err.message.contains("7 characters"));
    assert!(redact_typing_result(Ok(()), 7).is_ok());
}

#[test]
fn typed_mismatch_is_a_post_action_verification_failure() {
    let err = typed_mismatch();
    assert_eq!(err.code, ErrorCode::ActionFailed);
    assert_eq!(err.delivery, Delivery::DeliveredUnverified);
    assert_eq!(err.details.unwrap()["kind"], "post_action_verification");
    assert_eq!(err.suggestion.as_deref(), Some("use replace:true"));
}

#[tokio::test]
async fn force_param_is_plumbed_to_the_hotkey_policy() {
    assert!(!key_options(&json!({})).force);
    assert!(key_options(&json!({ "force": true })).force);

    let blocked = ghost_hotkey(json!({ "keys": blocked_keys(), "allow_hid": false }))
        .await
        .unwrap_err();
    assert_eq!(code_of(&blocked), "POLICY_DENIED");
    assert!(blocked.to_string().contains("is blocked"), "{blocked}");

    // With force the policy passes, so the refusal is now the physical-input gate.
    let forced = ghost_hotkey(json!({ "keys": blocked_keys(), "force": true, "allow_hid": false }))
        .await
        .unwrap_err();
    assert_eq!(ghost(&forced).code, ErrorCode::PolicyDenied);
    assert!(forced.to_string().contains("physical input"), "{forced}");

    let keys = blocked_keys();
    let (modifier, key) = (keys[0].clone(), keys[1].clone());
    let forced = ghost_press(
        json!({ "key": key, "modifiers": [modifier], "force": true, "allow_hid": false }),
    )
    .await
    .unwrap_err();
    assert!(forced.to_string().contains("physical input"), "{forced}");
}

#[tokio::test]
async fn blocked_hotkey_with_app_is_refused_before_focusing() {
    // Focusing an app settles for 100 ms; a refusal before focus returns at once.
    let started = Instant::now();
    let err = ghost_hotkey(json!({ "keys": blocked_keys(), "app": "GhostNoSuchApp" }))
        .await
        .unwrap_err();
    assert!(
        started.elapsed() < Duration::from_millis(90),
        "focused first"
    );
    assert_eq!(code_of(&err), "POLICY_DENIED");
    assert!(err.to_string().contains("is blocked"), "{err}");
}
