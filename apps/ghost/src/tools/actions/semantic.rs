//! Semantic element actions: press, focus, toggle, set a value, ... through the
//! platform accessibility backend (`AXTree::perform`), never through HID.
//!
//! The resolve-and-act runs inside one backend session in `ghost_eyes`; this module
//! only narrates it to the overlay and shapes the result. Callers own the HID
//! fallback decision (`allows_hid_fallback`).

use serde_json::{json, Value};

use ghost_eyes::{
    AXTree, ActionOptions, ActionOutcome, Delivery, ElementAction, ElementLocator, ErrorCode,
    GhostError, PlatformAXTree,
};

use super::disposition;
use crate::tools::overlay_events;
use crate::tools::refs::ResolvedRef;

/// Options for a Ghost action: renderer activation on (Chromium/Electron on macOS),
/// default deadline, focus steal allowed.
pub(super) fn action_options(verify: bool) -> ActionOptions {
    ActionOptions {
        verify,
        activate_renderer: true,
        ..ActionOptions::default()
    }
}

/// Act on a resolved ref. See [`act_on`].
pub async fn act(
    r: &ResolvedRef,
    action: &ElementAction,
    opts: &ActionOptions,
) -> Result<ActionOutcome, GhostError> {
    act_on(&r.locator, r.center, action, opts, "ghost_click").await
}

/// Perform `action` on the element `locator` names, bracketed by the overlay's
/// `down`/`up`/`done` events at `center` (skipped when the element has no bounds).
pub(super) async fn act_on(
    locator: &ElementLocator,
    center: Option<(i32, i32)>,
    action: &ElementAction,
    opts: &ActionOptions,
    tool: &str,
) -> Result<ActionOutcome, GhostError> {
    let ax = PlatformAXTree::new().map_err(|e| GhostError::from_anyhow(&e))?;
    if let Some((x, y)) = center {
        overlay_events::emit("down", x, y, tool);
    }
    let result = ax.perform(locator, action, opts).await;
    if let Some((x, y)) = center {
        overlay_events::emit("up", x, y, tool);
        overlay_events::emit("done", x, y, tool);
    }
    result
}

/// `{via:"semantic", semantic_method, disposition, verification, attempts}`.
pub fn outcome_json(outcome: &ActionOutcome) -> Value {
    json!({
        "via": "semantic",
        "semantic_method": outcome.method,
        "disposition": disposition(outcome.delivery),
        "verification": outcome.verification,
        "attempts": outcome.attempts,
    })
}

/// An HID fallback is allowed only after a semantic `ACTION_NOT_SUPPORTED` or
/// `PLATFORM_NOT_SUPPORTED` that delivered nothing.
pub(super) fn allows_hid_fallback(err: &GhostError) -> bool {
    matches!(
        err.code,
        ErrorCode::ActionNotSupported | ErrorCode::PlatformNotSupported
    ) && err.delivery == Delivery::NotDelivered
}
