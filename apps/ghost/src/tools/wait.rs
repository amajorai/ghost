use anyhow::Result;
use serde_json::{json, Value};
use tokio::time::{Duration, Instant};

use ghost_eyes::{AXTree, PlatformAXTree, PlatformWindowTracker, WindowTracker};

use super::{bounded_secs, f64_param, str_param};

/// Upper bound for `timeout` (and `interval`). It keeps `Instant::now() + timeout` from
/// overflowing and stops a single `delay` from tying up the stdio server for hours.
const MAX_WAIT_SECS: f64 = 300.0;
/// Floor for the poll `interval`, so `interval: 0` cannot spin a busy AX-polling loop.
const MIN_POLL_INTERVAL_SECS: f64 = 0.05;

pub async fn ghost_wait(params: Value) -> Result<Value> {
    let condition = params["condition"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("'condition' required"))?;
    let value = str_param(&params, "value");
    // Model-supplied numbers are untrusted: `Duration::from_secs_f64` panics on negative or
    // overflowing input, which would take down the whole MCP server mid-call. Reject a
    // negative timeout as a tool error the model can correct, and clamp everything else.
    let raw_timeout = f64_param(&params, "timeout", 10.0);
    if raw_timeout < 0.0 {
        anyhow::bail!("'timeout' must be a non-negative number of seconds");
    }
    let timeout = bounded_secs(raw_timeout, 10.0, 0.0, MAX_WAIT_SECS);
    let interval = bounded_secs(
        f64_param(&params, "interval", 0.5),
        0.5,
        MIN_POLL_INTERVAL_SECS,
        MAX_WAIT_SECS,
    );
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

        tokio::time::sleep(next_poll_sleep(interval, deadline, Instant::now())).await;
    }
}

/// Sleep until the next poll, but never past `deadline`: an `interval` longer than the
/// remaining timeout must not stretch the wait beyond the `timeout` the caller asked for.
fn next_poll_sleep(interval: Duration, deadline: Instant, now: Instant) -> Duration {
    interval.min(deadline.saturating_duration_since(now))
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

    #[tokio::test]
    async fn wait_rejects_negative_timeout_instead_of_panicking() {
        // `Duration::from_secs_f64(-1.0)` panics; the tool must return an error instead,
        // before any sleep or AX/window access.
        let err = ghost_wait(json!({ "condition": "delay", "timeout": -1 }))
            .await
            .expect_err("negative timeout is rejected");
        assert!(err.to_string().contains("non-negative"));
    }

    #[tokio::test]
    async fn wait_clamps_out_of_range_interval_instead_of_panicking() {
        // A negative or overflowing interval used to panic in `Duration::from_secs_f64`
        // before the delay branch ran; it is now clamped. A zero delay touches no AX state.
        for interval in [json!(-1), json!(1e300)] {
            let out =
                ghost_wait(json!({ "condition": "delay", "timeout": 0, "interval": interval }))
                    .await
                    .expect("out-of-range interval is clamped, not fatal");
            assert_eq!(out["condition_met"], json!(true));
            assert_eq!(out["elapsed_ms"], json!(0));
        }
    }

    #[test]
    fn next_poll_sleep_never_overshoots_the_deadline() {
        let now = Instant::now();
        let deadline = now + Duration::from_millis(200);
        // A long interval is cut to the time left before the deadline.
        assert_eq!(
            next_poll_sleep(Duration::from_secs(300), deadline, now),
            Duration::from_millis(200)
        );
        // A short interval is used as-is.
        assert_eq!(
            next_poll_sleep(Duration::from_millis(50), deadline, now),
            Duration::from_millis(50)
        );
        // Past the deadline there is nothing left to sleep.
        assert_eq!(
            next_poll_sleep(
                Duration::from_secs(1),
                deadline,
                deadline + Duration::from_secs(1)
            ),
            Duration::ZERO
        );
    }
}
