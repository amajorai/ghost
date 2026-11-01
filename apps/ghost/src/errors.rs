//! Ghost's MCP error envelope: maps any tool failure onto the shared
//! `ghost_eyes::contract` error codes and delivery dispositions.
//!
//! Classification never invents typed text or element values: messages come from
//! the error itself, and the `GhostError`/`KeyError` producers already keep secrets
//! out of their messages.

use serde_json::Value;

pub use ghost_eyes::{Delivery, ErrorCode, GhostError};

/// Legacy message starts (from validators written before the contract) that mean the
/// caller passed bad arguments. Matched only at the very start of the message.
const LEGACY_INVALID_ARGS_PREFIXES: &[&str] = &[
    "required",
    "Specify",
    "Unknown action",
    "must be",
    "Unknown key",
    "Unknown direction",
];

/// Every code a legacy `"<CODE>: message"` string may start with.
const ALL_CODES: &[ErrorCode] = &[
    ErrorCode::StaleRef,
    ErrorCode::AmbiguousTarget,
    ErrorCode::ElementNotFound,
    ErrorCode::ActionNotSupported,
    ErrorCode::ActionFailed,
    ErrorCode::PolicyDenied,
    ErrorCode::PermDenied,
    ErrorCode::Timeout,
    ErrorCode::InvalidArgs,
    ErrorCode::AppNotFound,
    ErrorCode::AppUnresponsive,
    ErrorCode::PlatformNotSupported,
    ErrorCode::UnknownTool,
    ErrorCode::Internal,
];

/// Whether a failure of `tool` may have changed the desktop (so its outcome is
/// unknown rather than safely "not delivered"). This is wider than
/// `dispatch::is_input_action`, which drives the input lock and is not widened.
pub fn may_mutate(tool: &str) -> bool {
    crate::mcp::dispatch::is_input_action(tool)
        || matches!(tool, "ghost_run" | "ghost_window" | "ghost_focus")
}

/// `{"delivery","retry"}` for a delivery state. Part of the §2.9 contract for success
/// results that report a disposition; currently exercised by tests only.
#[allow(dead_code)]
pub fn disposition_json(d: Delivery) -> Value {
    serde_json::to_value(ghost_eyes::Disposition::from(d)).unwrap_or(Value::Null)
}

/// Classify a tool failure into the shared error contract.
///
/// Order: a carried `GhostError`; a hotkey-policy `KeyError`; a click-plan
/// `PlanError`; legacy message prefixes; otherwise `ACTION_FAILED` (delivery unknown)
/// for tools that may mutate, and `INTERNAL` (not delivered) for read-only tools.
pub fn classify(err: &anyhow::Error, tool: &str) -> GhostError {
    if let Some(g) = typed_error(err) {
        return g;
    }
    let message = err.to_string();
    if let Some(g) = legacy_prefixed(&message, tool) {
        return g;
    }
    if may_mutate(tool) {
        GhostError::new(ErrorCode::ActionFailed, message).with_delivery(Delivery::Unknown)
    } else {
        GhostError::internal(message).with_delivery(Delivery::NotDelivered)
    }
}

/// A typed error anywhere in the `anyhow` chain (context layers included).
fn typed_error(err: &anyhow::Error) -> Option<GhostError> {
    for cause in err.chain() {
        if let Some(g) = cause.downcast_ref::<GhostError>() {
            return Some(g.clone());
        }
        if let Some(k) = cause.downcast_ref::<ghost_hands::KeyError>() {
            let code = code_from_str(k.code()).unwrap_or(ErrorCode::InvalidArgs);
            return Some(GhostError::new(code, k.message()));
        }
        if let Some(p) = cause.downcast_ref::<ghost_hands::mode::PlanError>() {
            return Some(GhostError::policy_denied(strip_any_code(&p.to_string())));
        }
    }
    None
}

fn legacy_prefixed(message: &str, tool: &str) -> Option<GhostError> {
    if let Some((code, rest)) = split_code_prefix(message) {
        return Some(GhostError::new(code, rest));
    }
    if message.starts_with("Unknown tool") {
        return Some(GhostError::new(ErrorCode::UnknownTool, message));
    }
    if tool != "ghost_run"
        && LEGACY_INVALID_ARGS_PREFIXES
            .iter()
            .any(|p| message.starts_with(p))
    {
        return Some(GhostError::invalid_args(message));
    }
    None
}

fn code_from_str(s: &str) -> Option<ErrorCode> {
    ALL_CODES.iter().copied().find(|c| c.as_str() == s)
}

/// `"<CODE>: rest"` (CODE a known contract code) → `(code, "rest")`.
fn split_code_prefix(message: &str) -> Option<(ErrorCode, &str)> {
    let (head, rest) = message.split_once(':')?;
    let code = code_from_str(head)?;
    Some((code, rest.trim_start()))
}

fn strip_any_code(message: &str) -> String {
    split_code_prefix(message).map_or_else(|| message.to_string(), |(_, rest)| rest.to_string())
}

#[cfg(test)]
#[path = "errors_test.rs"]
mod tests;
