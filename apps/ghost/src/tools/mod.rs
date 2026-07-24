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
}
