//! Keyboard tools: press a key, send a hotkey.
//!
//! The hotkey policy (`ghost_hands::check_key`/`check_hotkey`) runs before any app
//! is focused, so a blocked combo is refused without side effects. `force` is the
//! only way past it and is meant only for shortcuts the user explicitly asked for.

use anyhow::Result;
use serde_json::{json, Value};

use ghost_eyes::GhostError;
use ghost_hands::{check_hotkey, check_key, press_key_with, send_hotkey_with, KeyOptions};

use super::{focus_requested_app, hid_disposition, require_hid};
use crate::tools::{bool_param, str_param};

/// The `force` param (default false) as hotkey-policy options.
pub(super) fn key_options(params: &Value) -> KeyOptions {
    KeyOptions {
        force: bool_param(params, "force", false),
    }
}

pub async fn ghost_press(params: Value) -> Result<Value> {
    let key = params["key"]
        .as_str()
        .ok_or_else(|| GhostError::invalid_args("'key' required"))?;
    let modifiers: Vec<String> = params["modifiers"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let opts = key_options(&params);

    // Policy and physical-input checks come before any focus.
    let mod_refs: Vec<&str> = modifiers.iter().map(String::as_str).collect();
    check_key(key, &mod_refs, opts)?;
    require_hid(&params)?;
    focus_requested_app(str_param(&params, "app")).await;

    let key_owned = key.to_string();
    tokio::task::spawn_blocking(move || {
        let mod_refs: Vec<&str> = modifiers.iter().map(String::as_str).collect();
        press_key_with(&key_owned, &mod_refs, opts)
    })
    .await??;
    Ok(json!({ "success": true, "key": key, "via": "hid", "disposition": hid_disposition() }))
}

pub async fn ghost_hotkey(params: Value) -> Result<Value> {
    let keys: Vec<String> = params["keys"]
        .as_array()
        .ok_or_else(|| GhostError::invalid_args("'keys' required (array)"))?
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    let opts = key_options(&params);

    // Policy and physical-input checks come before any focus. An empty list is a
    // no-op, as before.
    let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
    let sends_input = check_hotkey(&refs, opts)?.is_some();
    if sends_input {
        require_hid(&params)?;
    }
    focus_requested_app(str_param(&params, "app")).await;

    tokio::task::spawn_blocking(move || {
        let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
        send_hotkey_with(&refs, opts)
    })
    .await??;
    let mut out = json!({ "success": true, "keys": params["keys"] });
    if sends_input {
        out["via"] = json!("hid");
        out["disposition"] = hid_disposition();
    }
    Ok(out)
}
