//! `ghost_type`: type text, optionally into a ref, an `into` query or a CDP field.
//!
//! With a ref, `replace:true` sets the whole value semantically (no keystrokes) and
//! verifies it. Otherwise the field is focused semantically (an HID click only when
//! that is unsupported) and the text is typed physically; `verify:true` reads the
//! field back. Typed text is never echoed or put in an error: results carry
//! `typed_chars`, and typing failures are replaced by a generic message.

use std::time::Duration;

use anyhow::Result;
use serde_json::{json, Value};

use ghost_eyes::accessibility::verify::verify_typed;
use ghost_eyes::{
    AXTree, Delivery, ElementAction, ElementLocator, ErrorCode, GhostError, PlatformAXTree,
    Verification, VerificationScope,
};
use ghost_hands::type_text;

use super::pointer::left_click;
use super::semantic::{act_on, action_options, allows_hid_fallback, outcome_json};
use super::targets::{ax_target, cdp_target, dom_selectors};
use super::{focus_requested_app, hid_allowed, hid_denied, hid_disposition, settle};
use crate::tools::overlay_events;
use crate::tools::refs::{resolve_ref, ResolvedRef};
use crate::tools::{bool_param, nonblank_str_param, str_param};

/// Settle time before reading a field back after typing.
const READBACK_SETTLE: Duration = Duration::from_millis(150);
/// How long the read-back resolution may take.
const READBACK_DEADLINE: Duration = Duration::from_secs(3);

pub async fn ghost_type(params: Value) -> Result<Value> {
    let text = params["text"]
        .as_str()
        .ok_or_else(|| GhostError::invalid_args("'text' required"))?;
    // A blank `into` would match the root AX node and click the window centre.
    let into = nonblank_str_param(&params, "into");
    let clear = bool_param(&params, "clear", false);
    let replace = bool_param(&params, "replace", false);
    let reference = str_param(&params, "ref");
    if replace && reference.is_none() {
        return Err(GhostError::invalid_args("replace:true needs a ref").into());
    }
    // Everything but a semantic replace types physically: refuse before focusing.
    if !replace && !hid_allowed(&params) {
        return Err(hid_denied().into());
    }
    let chars = text.chars().count();

    focus_requested_app(str_param(&params, "app")).await;

    if let Some(r) = reference {
        let resolved = resolve_ref(r, str_param(&params, "snapshot")).await?;
        let mut out = if replace {
            replace_value(&resolved, text).await?
        } else {
            let verify = bool_param(&params, "verify", false);
            type_into_ref(&resolved, text, clear, verify).await?
        };
        out["success"] = json!(true);
        out["typed_chars"] = json!(chars);
        out["ref"] = json!(r);
        out["method"] = json!("ref");
        if !resolved.qualified {
            out["ref_resolved_against"] = json!(resolved.snapshot_id);
        }
        return Ok(out);
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
    Ok(json!({
        "success": true, "typed_chars": chars, "via": "hid", "disposition": hid_disposition(),
    }))
}

/// Set the field's whole value semantically and verify it.
async fn replace_value(resolved: &ResolvedRef, text: &str) -> Result<Value, GhostError> {
    let action = ElementAction::SetValue(text.to_string());
    let opts = action_options(true);
    let outcome = act_on(
        &resolved.locator,
        resolved.center,
        &action,
        &opts,
        "ghost_type",
    )
    .await?;
    Ok(outcome_json(&outcome))
}

/// Focus the field (semantically, else an HID click), type, and optionally verify.
/// The caller has already checked that physical input is allowed.
async fn type_into_ref(
    resolved: &ResolvedRef,
    text: &str,
    clear: bool,
    verify: bool,
) -> Result<Value, GhostError> {
    let element = &resolved.element;
    let secure = element.node.secure || element.state.secure;
    let before = if verify && !secure {
        element
            .state
            .value
            .clone()
            .or_else(|| element.node.value.clone())
    } else {
        None
    };

    let focus = ElementAction::Focus;
    let opts = action_options(false);
    let focus_via = match act_on(
        &resolved.locator,
        resolved.center,
        &focus,
        &opts,
        "ghost_type",
    )
    .await
    {
        Ok(_) => "semantic",
        Err(e) if allows_hid_fallback(&e) => {
            let Some((x, y)) = resolved.center else {
                return Err(e);
            };
            left_click(x, y).await.map_err(|_| {
                GhostError::new(ErrorCode::ActionFailed, "clicking the field failed")
            })?;
            "hid"
        }
        Err(e) => return Err(e),
    };
    settle().await;

    let (x, y) = resolved.center.unwrap_or((0, 0));
    type_with_overlay(text, clear, x, y).await?;

    let mut out = json!({ "via": "hid", "focus_via": focus_via, "disposition": hid_disposition() });
    if verify {
        let verification = if secure {
            unavailable("secure_field")
        } else {
            verify_after_typing(&resolved.locator, before.as_deref(), text, clear).await?
        };
        out["verification"] = json!(verification);
    }
    Ok(out)
}

/// Read the field back and compare it with what was typed.
async fn verify_after_typing(
    locator: &ElementLocator,
    before: Option<&str>,
    text: &str,
    clear: bool,
) -> Result<Verification, GhostError> {
    tokio::time::sleep(READBACK_SETTLE).await;
    let Some(after) = read_back(locator).await else {
        return Ok(unavailable("insufficient_evidence"));
    };
    let after = match after {
        ReadBack::Secure => return Ok(unavailable("secure_field")),
        ReadBack::Value(v) => v,
    };
    match verify_typed(before, text, clear, after.as_deref()) {
        Some(true) => Ok(Verification {
            scope: VerificationScope::ElementValue,
            reason: None,
        }),
        Some(false) => Err(typed_mismatch()),
        None => Ok(unavailable("insufficient_evidence")),
    }
}

/// ACTION_FAILED after the keystrokes were sent but the field does not hold the text.
pub(super) fn typed_mismatch() -> GhostError {
    GhostError::new(
        ErrorCode::ActionFailed,
        "the field's value after typing does not match the typed text",
    )
    .with_delivery(Delivery::DeliveredUnverified)
    .with_details(json!({ "kind": "post_action_verification" }))
    .with_suggestion("use replace:true")
}

enum ReadBack {
    Secure,
    Value(Option<String>),
}

async fn read_back(locator: &ElementLocator) -> Option<ReadBack> {
    let ax = PlatformAXTree::new().ok()?;
    let el = ax.resolve(locator, READBACK_DEADLINE).await.ok()?;
    if el.node.secure || el.state.secure {
        return Some(ReadBack::Secure);
    }
    Some(ReadBack::Value(el.state.value.or(el.node.value)))
}

fn unavailable(reason: &'static str) -> Verification {
    Verification {
        scope: VerificationScope::Unavailable,
        reason: Some(reason),
    }
}

/// Type `text`, bracketed by the overlay's typing events at `(x, y)`.
async fn type_with_overlay(text: &str, clear: bool, x: i32, y: i32) -> Result<(), GhostError> {
    let chars = text.chars().count();
    let text_owned = text.to_string();
    overlay_events::emit("type", x, y, "ghost_type");
    let joined = tokio::task::spawn_blocking(move || type_text(&text_owned, clear)).await;
    overlay_events::emit("done", x, y, "ghost_type");
    redact_typing_result(joined.map_err(anyhow::Error::from).and_then(|r| r), chars)
}

/// Replace any typing error with a generic ACTION_FAILED (delivery unknown): the
/// platform error text can contain the characters that were being typed.
pub(super) fn redact_typing_result(result: Result<()>, chars: usize) -> Result<(), GhostError> {
    result.map_err(|_| {
        GhostError::new(
            ErrorCode::ActionFailed,
            format!("typing failed ({chars} characters)"),
        )
        .with_delivery(Delivery::Unknown)
    })
}
