pub mod actions;
pub mod annotate;
pub mod journal;
pub mod learning;
pub mod overlay_events;
pub mod perception;
pub mod recipes;
pub mod snapshot;
pub mod vision;
pub mod wait;

use serde_json::Value;

/// Extract a string param, returning a helpful error if required and missing.
pub fn str_param<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params[key].as_str()
}

/// A non-blank string param, trimmed. A missing, non-string or whitespace-only value is
/// `None`, so an explicit `""` is treated the same as an absent locator.
pub fn nonblank_str_param<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    str_param(params, key)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// The `query` locator for an element-targeting tool. Rejects a missing or blank query:
/// an empty query matches the root accessibility node, which would act on the centre of
/// the focused window and report success. When `dom_id`/`dom_class` was given but did
/// not resolve (and no query is present), the error names that selector instead.
pub fn required_query<'a>(params: &'a Value, locators: &str) -> anyhow::Result<&'a str> {
    if let Some(query) = nonblank_str_param(params, "query") {
        return Ok(query);
    }
    if let Some(id) = str_param(params, "dom_id") {
        anyhow::bail!("dom_id '#{id}' not found or Chrome DevTools (CDP) unavailable");
    }
    if let Some(class) = str_param(params, "dom_class") {
        anyhow::bail!("dom_class '.{class}' not found or Chrome DevTools (CDP) unavailable");
    }
    anyhow::bail!("Specify {locators} to identify the target.")
}

/// Extract a bool param with default.
pub fn bool_param(params: &Value, key: &str, default: bool) -> bool {
    params[key].as_bool().unwrap_or(default)
}

/// Extract an i64 param with default.
pub fn int_param(params: &Value, key: &str, default: i64) -> i64 {
    params[key].as_i64().unwrap_or(default)
}

/// Extract a f64 param with default.
pub fn f64_param(params: &Value, key: &str, default: f64) -> f64 {
    params[key].as_f64().unwrap_or(default)
}

/// Convert float seconds to a Duration without panicking: a non-finite value falls back
/// to `default`, and the result is clamped to `min..=max`. Callers must pass a finite
/// `0 <= min <= max` (a few hundred seconds at most) so `from_secs_f64` cannot overflow.
pub fn bounded_secs(secs: f64, default: f64, min: f64, max: f64) -> std::time::Duration {
    let secs = if secs.is_finite() { secs } else { default };
    std::time::Duration::from_secs_f64(secs.clamp(min, max))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn str_param_returns_string_or_none() {
        let v = json!({ "a": "hello", "b": 3 });
        assert_eq!(str_param(&v, "a"), Some("hello"));
        // Present but not a string.
        assert_eq!(str_param(&v, "b"), None);
        // Absent.
        assert_eq!(str_param(&v, "missing"), None);
    }

    #[test]
    fn required_query_rejects_missing_or_blank_queries() {
        assert_eq!(
            required_query(&json!({ "query": "  Submit " }), "x/y").unwrap(),
            "Submit"
        );
        for params in [json!({}), json!({ "query": "" }), json!({ "query": "   " })] {
            let err = required_query(&params, "x/y or a query").unwrap_err();
            assert!(err.to_string().contains("Specify x/y or a query"), "{err}");
        }
        let err = required_query(&json!({ "dom_id": "submit" }), "x/y").unwrap_err();
        assert!(
            err.to_string().contains("dom_id '#submit' not found"),
            "{err}"
        );
        let err = required_query(&json!({ "dom_class": "btn" }), "x/y").unwrap_err();
        assert!(
            err.to_string().contains("dom_class '.btn' not found"),
            "{err}"
        );
    }

    #[test]
    fn bool_param_uses_default_when_absent_or_wrong_type() {
        let v = json!({ "flag": true, "notbool": "yes" });
        assert!(bool_param(&v, "flag", false));
        assert!(bool_param(&v, "notbool", true)); // wrong type → default
        assert!(!bool_param(&v, "absent", false));
    }

    #[test]
    fn int_param_uses_default_when_absent_or_wrong_type() {
        let v = json!({ "n": 7, "s": "12" });
        assert_eq!(int_param(&v, "n", 0), 7);
        assert_eq!(int_param(&v, "s", -1), -1); // string is not an i64 → default
        assert_eq!(int_param(&v, "absent", 99), 99);
    }

    #[test]
    fn f64_param_uses_default_when_absent_or_wrong_type() {
        let v = json!({ "x": 2.5, "s": "3.0" });
        assert_eq!(f64_param(&v, "x", 0.0), 2.5);
        assert_eq!(f64_param(&v, "s", 1.0), 1.0); // string is not an f64 → default
        assert_eq!(f64_param(&v, "absent", 4.0), 4.0);
    }

    #[test]
    fn bounded_secs_never_panics() {
        use std::time::Duration;
        // Negative input (which `Duration::from_secs_f64` panics on) clamps to the floor.
        assert_eq!(bounded_secs(-1.0, 1.0, 0.0, 300.0), Duration::ZERO);
        // Huge input (overflowing `Duration`) clamps to the ceiling.
        assert_eq!(
            bounded_secs(1e300, 1.0, 0.0, 300.0),
            Duration::from_secs(300)
        );
        assert_eq!(
            bounded_secs(1e19, 1.0, 0.0, 300.0),
            Duration::from_secs(300)
        );
        // Non-finite input falls back to the default.
        assert_eq!(
            bounded_secs(f64::NAN, 1.0, 0.0, 300.0),
            Duration::from_secs(1)
        );
        assert_eq!(
            bounded_secs(f64::INFINITY, 1.0, 0.0, 300.0),
            Duration::from_secs(1)
        );
        // In-range input passes through unchanged.
        assert_eq!(
            bounded_secs(2.5, 1.0, 0.0, 300.0),
            Duration::from_millis(2500)
        );
    }
}
