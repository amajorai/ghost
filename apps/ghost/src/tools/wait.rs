use anyhow::Result;
use serde_json::{json, Value};
use tokio::time::{Duration, Instant};

use ghost_eyes::{AXTree, PlatformAXTree, PlatformWindowTracker, WindowTracker};

use super::{f64_param, str_param};

pub async fn ghost_wait(params: Value) -> Result<Value> {
    let condition = params["condition"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("'condition' required"))?;
    let value = str_param(&params, "value");
    let timeout = Duration::from_secs_f64(f64_param(&params, "timeout", 10.0));
    let interval = Duration::from_secs_f64(f64_param(&params, "interval", 0.5));
    let _app = str_param(&params, "app");

    // "delay" is a one-shot sleep, not a polled condition
    if condition == "delay" {
        tokio::time::sleep(timeout).await;
        return Ok(json!({
            "condition_met": true,
            "condition": "delay",
            "polls": 0,
            "elapsed_ms": timeout.as_millis() as u64,
        }));
    }

    let deadline = Instant::now() + timeout;

    // Capture initial state for "Changed" conditions
    let initial_url = get_current_url().await;
    let initial_title = get_current_title().await;

    let mut poll_count = 0u32;
    loop {
        poll_count += 1;
        let met = check_condition(condition, value, &initial_url, &initial_title).await;

        if met {
            return Ok(json!({
                "condition_met": true,
                "condition": condition,
                "polls": poll_count,
                "elapsed_ms": (Instant::now() - (deadline - timeout)).as_millis() as u64,
            }));
        }

        if Instant::now() >= deadline {
            return Ok(json!({
                "condition_met": false,
                "condition": condition,
                "timed_out": true,
                "polls": poll_count,
                "suggestion": "The condition was not met within the timeout. Try increasing 'timeout' or verify the condition string."
            }));
        }

        tokio::time::sleep(interval).await;
    }
}

async fn check_condition(
    condition: &str,
    value: Option<&str>,
    initial_url: &Option<String>,
    initial_title: &Option<String>,
) -> bool {
    match condition {
        "urlContains" => {
            let url = get_current_url().await;
            let v = value.unwrap_or("");
            url.as_deref().unwrap_or("").contains(v)
        }
        "titleContains" => {
            let title = get_current_title().await;
            let v = value.unwrap_or("");
            title.as_deref().unwrap_or("").contains(v)
        }
        "elementExists" => {
            let query = value.unwrap_or("");
            if query.is_empty() {
                return false;
            }
            let ax = match PlatformAXTree::new() {
                Ok(a) => a,
                Err(_) => return false,
            };
            ax.find_element(query).await.is_some()
        }
        "elementGone" => {
            let query = value.unwrap_or("");
            if query.is_empty() {
                return false;
            }
            let ax = match PlatformAXTree::new() {
                Ok(a) => a,
                Err(_) => return false,
            };
            ax.find_element(query).await.is_none()
        }
        "urlChanged" => {
            let current = get_current_url().await;
            current != *initial_url
        }
        "titleChanged" => {
            let current = get_current_title().await;
            current != *initial_title
        }
        _ => false,
    }
}

async fn get_current_url() -> Option<String> {
    let tracker = PlatformWindowTracker::new().ok()?;
    tracker.get_active_window().await?.url
}

async fn get_current_title() -> Option<String> {
    let tracker = PlatformWindowTracker::new().ok()?;
    Some(tracker.get_active_window().await?.title)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wait_requires_a_condition() {
        let err = ghost_wait(json!({}))
            .await
            .expect_err("condition is required");
        assert!(err.to_string().contains("'condition' required"));
    }

    #[tokio::test]
    async fn check_condition_unknown_condition_is_never_met() {
        // The default arm returns false without querying any window/AX state.
        assert!(!check_condition("bogusCondition", None, &None, &None).await);
    }

    #[tokio::test]
    async fn wait_delay_is_a_one_shot_sleep_that_reports_met() {
        // A zero-length delay returns immediately without polling any AX/window state.
        let out = ghost_wait(json!({ "condition": "delay", "timeout": 0.0 }))
            .await
            .expect("delay succeeds");
        assert_eq!(out["condition_met"], json!(true));
        assert_eq!(out["condition"], json!("delay"));
        assert_eq!(out["polls"], json!(0));
    }
}
