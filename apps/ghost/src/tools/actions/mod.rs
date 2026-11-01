//! Input action tools: click, type, keys, scroll, hover, long press, drag, and
//! window management. Each handler validates its params before touching the
//! accessibility tree or focusing an app.
//!
//! Element targets (refs and queries) are acted on semantically first, through the
//! platform accessibility backend (`semantic`). Physical (HID) input runs only when
//! the caller allows it (`allow_hid`, default true) and the session has a desktop
//! (`ghost_hands::mode::hid_permitted`); otherwise it is refused with POLICY_DENIED.

mod click;
mod keys;
mod pointer;
mod semantic;
mod targets;
mod typing;
mod window;

pub use click::ghost_click;
pub use keys::{ghost_hotkey, ghost_press};
pub use pointer::{ghost_drag, ghost_hover, ghost_long_press, ghost_scroll};
pub use typing::ghost_type;
pub use window::{ghost_focus, ghost_window};

use serde_json::Value;

use ghost_eyes::{Delivery, Disposition, GhostError};
use ghost_hands::focus_app;
use ghost_hands::mode::hid_permitted;

use crate::tools::bool_param;

/// The suggestion every physical-input refusal carries.
const HID_SUGGESTION: &str = "use replace:true, or retry with allow_hid:true on a desktop session";

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

/// Whether this call may synthesize physical input: `allow_hid` (default true) and
/// not a headless session.
fn hid_allowed(params: &Value) -> bool {
    hid_permitted(bool_param(params, "allow_hid", true))
}

/// POLICY_DENIED for physical input that is not allowed here.
fn hid_denied() -> GhostError {
    GhostError::policy_denied(
        "physical input is not allowed here (allow_hid is false or the session is headless)",
    )
    .with_suggestion(HID_SUGGESTION)
}

/// Refuse unless physical input is allowed for this call.
fn require_hid(params: &Value) -> Result<(), GhostError> {
    if hid_allowed(params) {
        Ok(())
    } else {
        Err(hid_denied())
    }
}

/// `{"delivery","retry"}` for a delivery state.
fn disposition(delivery: Delivery) -> Value {
    serde_json::to_value(Disposition::from(delivery)).unwrap_or(Value::Null)
}

/// The disposition of an HID-only success: sent, but nothing read back.
fn hid_disposition() -> Value {
    disposition(Delivery::DeliveredUnverified)
}

#[cfg(test)]
mod tests;
