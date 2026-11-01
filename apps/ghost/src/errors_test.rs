//! Classification tests for the MCP error envelope.

use super::*;
use ghost_eyes::Retry;
use serde_json::json;

fn classify_msg(message: &str, tool: &str) -> GhostError {
    classify(&anyhow::anyhow!(message.to_string()), tool)
}

#[test]
fn a_carried_ghost_error_is_returned_unchanged() {
    let original = GhostError::stale_ref("ref @e4 is gone")
        .with_suggestion("re-snapshot")
        .with_details(json!({ "reason": "resolution_complete_absence" }));
    let err = anyhow::Error::from(original.clone());
    assert_eq!(classify(&err, "ghost_click"), original);
}

#[test]
fn a_ghost_error_under_context_is_still_found() {
    let err = anyhow::Error::from(GhostError::invalid_args("bad ref")).context("while clicking");
    let g = classify(&err, "ghost_click");
    assert_eq!(g.code, ErrorCode::InvalidArgs);
    assert_eq!(g.message, "bad ref");
}

#[test]
fn a_key_error_maps_to_its_code_and_message_without_prefix() {
    let err = anyhow::Error::new(ghost_hands::KeyError::UnknownKey("zz".into()));
    let g = classify(&err, "ghost_press");
    assert_eq!(g.code, ErrorCode::InvalidArgs);
    assert_eq!(g.message, "Unknown key: 'zz'");
}

#[test]
fn a_blocked_key_error_renders_exactly_one_policy_denied_prefix() {
    let err = anyhow::Error::new(ghost_hands::KeyError::Blocked {
        combo: "alt+f4".into(),
        reason: "closes the window",
    });
    let g = classify(&err, "ghost_hotkey");
    assert_eq!(g.code, ErrorCode::PolicyDenied);
    assert_eq!(g.delivery, Delivery::NotDelivered);
    let text = format!("Error: {g}");
    assert!(
        text.starts_with("Error: POLICY_DENIED: key combo 'alt+f4'"),
        "{text}"
    );
    assert_eq!(text.matches("POLICY_DENIED").count(), 1, "{text}");
}

#[test]
fn a_plan_error_is_policy_denied_without_a_double_prefix() {
    let err = anyhow::Error::new(ghost_hands::mode::PlanError::HidNotAllowed);
    let g = classify(&err, "ghost_click");
    assert_eq!(g.code, ErrorCode::PolicyDenied);
    assert_eq!(g.message, "physical input is not allowed here");
    assert_eq!(g.to_string().matches("POLICY_DENIED").count(), 1);
}

#[test]
fn legacy_stale_ref_prefix_becomes_the_code_and_is_stripped() {
    let g = classify_msg("STALE_REF: @e9 not found in the current UI", "ghost_click");
    assert_eq!(g.code, ErrorCode::StaleRef);
    assert_eq!(g.message, "@e9 not found in the current UI");
    assert_eq!(g.delivery, Delivery::NotDelivered);
}

#[test]
fn legacy_ambiguous_prefix_is_not_doubled() {
    let g = classify_msg("AMBIGUOUS_TARGET: 2 candidates", "ghost_type");
    assert_eq!(g.code, ErrorCode::AmbiguousTarget);
    assert_eq!(g.to_string(), "AMBIGUOUS_TARGET: 2 candidates");
}

#[test]
fn unknown_tool_is_classified() {
    let g = classify_msg("Unknown tool: ghost_nope", "ghost_nope");
    assert_eq!(g.code, ErrorCode::UnknownTool);
    assert_eq!(g.message, "Unknown tool: ghost_nope");
}

#[test]
fn legacy_invalid_args_prefixes_are_matched_at_the_start_only() {
    for msg in [
        "required: 'text'",
        "Specify query, ref, or x/y",
        "Unknown action: fly",
        "must be a number",
        "Unknown key: hyper",
        "Unknown direction: sideways",
    ] {
        assert_eq!(
            classify_msg(msg, "ghost_click").code,
            ErrorCode::InvalidArgs,
            "{msg}"
        );
    }
}

#[test]
fn a_mid_message_required_is_not_invalid_args() {
    let g = classify_msg("'condition' required", "ghost_context");
    assert_eq!(g.code, ErrorCode::Internal);
    let g = classify_msg("the app said a password is required", "ghost_click");
    assert_eq!(g.code, ErrorCode::ActionFailed);
}

#[test]
fn ghost_run_is_never_legacy_classified_as_invalid_args() {
    // A recipe step's own failure text must not look like a bad ghost_run call.
    for msg in [
        "required field missing in step 3",
        "Specify a value",
        "must be visible",
    ] {
        let g = classify_msg(msg, "ghost_run");
        assert_eq!(g.code, ErrorCode::ActionFailed, "{msg}");
        assert_eq!(g.delivery, Delivery::Unknown);
    }
}

#[test]
fn mutating_tools_default_to_action_failed_with_unknown_delivery() {
    for tool in [
        "ghost_click",
        "ghost_type",
        "ghost_press",
        "ghost_hotkey",
        "ghost_scroll",
        "ghost_hover",
        "ghost_long_press",
        "ghost_drag",
        "ghost_run",
        "ghost_window",
        "ghost_focus",
    ] {
        assert!(may_mutate(tool), "{tool}");
        let g = classify_msg("the OS refused", tool);
        assert_eq!(g.code, ErrorCode::ActionFailed, "{tool}");
        assert_eq!(g.delivery, Delivery::Unknown, "{tool}");
        assert_eq!(g.disposition().retry, Retry::Unknown, "{tool}");
    }
}

#[test]
fn read_only_tools_default_to_internal_not_delivered() {
    for tool in [
        "ghost_context",
        "ghost_find",
        "ghost_snapshot",
        "ghost_wait",
        "ghost_recipes",
    ] {
        assert!(!may_mutate(tool), "{tool}");
        let g = classify_msg("the OS refused", tool);
        assert_eq!(g.code, ErrorCode::Internal, "{tool}");
        assert_eq!(g.delivery, Delivery::NotDelivered, "{tool}");
        assert_eq!(g.disposition().retry, Retry::Safe, "{tool}");
    }
}

#[test]
fn may_mutate_does_not_widen_the_input_lock() {
    for tool in ["ghost_run", "ghost_window", "ghost_focus"] {
        assert!(!crate::mcp::dispatch::is_input_action(tool), "{tool}");
    }
}

#[test]
fn disposition_json_derives_retry_from_delivery() {
    assert_eq!(
        disposition_json(Delivery::NotDelivered),
        json!({ "delivery": "not_delivered", "retry": "safe" })
    );
    assert_eq!(
        disposition_json(Delivery::DeliveredUnverified),
        json!({ "delivery": "delivered_unverified", "retry": "unsafe" })
    );
    assert_eq!(
        disposition_json(Delivery::Unknown),
        json!({ "delivery": "unknown", "retry": "unknown" })
    );
}

#[test]
fn error_json_has_the_contract_shape() {
    let g = classify_msg("STALE_REF: gone", "ghost_click");
    let v = g.to_json();
    assert_eq!(v["code"], "STALE_REF");
    assert_eq!(v["message"], "gone");
    assert_eq!(
        v["disposition"],
        json!({ "delivery": "not_delivered", "retry": "safe" })
    );
}
