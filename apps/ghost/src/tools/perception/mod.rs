//! Perception tools: context, state, find, read, inspect, element-at and
//! screenshot. Tree helpers (matching, collection) live in `walk`; every node
//! is serialized through `redact`, which withholds secure (password) values.

mod redact;
mod walk;

use anyhow::Result;
use base64::Engine;
use serde_json::{json, Value};

use ghost_eyes::{quick_screenshot, AXTree, PlatformAXTree, PlatformWindowTracker, WindowTracker};

use super::{bool_param, int_param, required_query, str_param};
use walk::{
    collect_interactive, element_at_json, find_elements, focused_element, inspect_json, read_lines,
};

pub async fn ghost_context(params: Value) -> Result<Value> {
    let _app = str_param(&params, "app");

    let tracker = PlatformWindowTracker::new()?;
    let win = tracker.get_active_window().await;
    let ax = PlatformAXTree::new()?;
    let tree = ax.get_focused_tree().await.ok();

    let interactive = tree
        .as_ref()
        .map(|t| collect_interactive(t, 0, 3))
        .unwrap_or_default();

    Ok(json!({
        "app":       win.as_ref().map(|w| w.app_name.as_str()),
        "bundle_id": win.as_ref().and_then(|w| w.bundle_id.as_deref()),
        "pid":       win.as_ref().map(|w| w.pid),
        "title":     win.as_ref().map(|w| w.title.as_str()),
        "url":       win.as_ref().and_then(|w| w.url.as_deref()),
        "focused_element": tree.as_ref().map(focused_element),
        "interactive_count": interactive.len(),
        "interactive": interactive,
        "suggestion": "Use ghost_find to locate specific elements, ghost_click to act."
    }))
}

pub async fn ghost_state(params: Value) -> Result<Value> {
    let app_filter = str_param(&params, "app");

    let tracker = PlatformWindowTracker::new()?;
    let active = tracker.get_active_window().await;
    let ax = PlatformAXTree::new()?;
    let apps = ax.list_apps().await;

    let windows: Vec<Value> = if let Some(win) = &active {
        if app_filter
            .map(|f| win.app_name.to_lowercase().contains(&f.to_lowercase()))
            .unwrap_or(true)
        {
            vec![json!({
                "app":       win.app_name,
                "bundle_id": win.bundle_id,
                "title":     win.title,
                "url":       win.url,
                "pid":       win.pid,
                "focused":   true,
            })]
        } else {
            vec![]
        }
    } else {
        vec![]
    };

    Ok(json!({ "apps": apps, "windows": windows }))
}

pub async fn ghost_find(params: Value) -> Result<Value> {
    let query = str_param(&params, "query");
    let role = str_param(&params, "role");
    let dom_id = str_param(&params, "dom_id");
    let identifier = str_param(&params, "identifier");
    let dom_class = str_param(&params, "dom_class");
    let depth = int_param(&params, "depth", 25).min(100) as u32;

    let ax = PlatformAXTree::new()?;
    let tree = ax.get_focused_tree().await?;

    let q = query.unwrap_or("").to_lowercase();
    let r = role.unwrap_or("").to_lowercase();
    let dom_id_str = dom_id.unwrap_or("").to_lowercase();
    let id = if let Some(ident) = identifier {
        ident.to_lowercase()
    } else {
        dom_id_str.clone()
    };
    let cls = dom_class.unwrap_or("").to_lowercase();

    let mut results = vec![];
    find_elements(&tree, &q, &r, &id, &cls, depth, &mut results);

    // CDP fallback: if AX found nothing and Chrome debug port is open, try JS matching.
    // Also triggered when dom_class or dom_id is specified.
    if results.is_empty()
        && (!q.is_empty() || !cls.is_empty() || !dom_id_str.is_empty())
        && ghost_core::cdp::is_available().await
    {
        // For dom_class, pass as a CSS selector (".className"); for dom_id pass "#id"
        let cdp_query = if !cls.is_empty() && q.is_empty() {
            format!(".{}", cls)
        } else if !dom_id_str.is_empty() && q.is_empty() {
            format!("#{}", dom_id_str)
        } else {
            query.unwrap_or("").to_string()
        };
        if let Ok(cdp_els) = ghost_core::cdp::find_elements(&cdp_query).await {
            for el in cdp_els {
                results.push(json!({
                    "role":   el.tag,
                    "title":  el.text,
                    "x":      el.center_x as i32,
                    "y":      el.center_y as i32,
                    "source": "cdp",
                    "match_type": el.match_type,
                }));
            }
        }
    }

    Ok(json!({
        "count":    results.len(),
        "elements": results,
        "suggestion": if results.is_empty() { "No elements found. Try ghost_annotate for a visual view or ghost_read to see all text." } else { "Use ghost_click with x/y coordinates from the bounds field." }
    }))
}

pub async fn ghost_read(params: Value) -> Result<Value> {
    let query = str_param(&params, "query");
    let depth = int_param(&params, "depth", 25) as u32;

    let ax = PlatformAXTree::new()?;
    let tree = ax.get_focused_tree().await?;

    let texts = read_lines(&tree, query.unwrap_or(""), depth);

    Ok(json!({ "text": texts.join("\n"), "lines": texts }))
}

pub async fn ghost_inspect(params: Value) -> Result<Value> {
    // dom_id: CDP #id lookup
    let dom_id = str_param(&params, "dom_id");
    if let Some(id) = &dom_id {
        if ghost_core::cdp::is_available().await {
            if let Ok(els) = ghost_core::cdp::find_elements(&format!("#{id}")).await {
                if let Some(el) = els.first() {
                    return Ok(json!({
                        "role": el.tag, "title": el.text, "match_type": el.match_type,
                        "x": el.center_x as i32, "y": el.center_y as i32,
                        "source": "cdp", "actionable": true,
                    }));
                }
            }
        }
    }

    // dom_class: CDP .class lookup
    let dom_class = str_param(&params, "dom_class");
    if let Some(cls) = &dom_class {
        if ghost_core::cdp::is_available().await {
            if let Ok(els) = ghost_core::cdp::find_elements(&format!(".{cls}")).await {
                if let Some(el) = els.first() {
                    return Ok(json!({
                        "role": el.tag, "title": el.text, "match_type": el.match_type,
                        "x": el.center_x as i32, "y": el.center_y as i32,
                        "source": "cdp", "actionable": true,
                    }));
                }
            }
        }
    }

    // A blank query would match the root AX node and report the whole window.
    let query = required_query(&params, "query or dom_id/dom_class")?;
    let ax = PlatformAXTree::new()?;
    let element = ax.find_element(query).await;

    match element {
        Some(el) => Ok(inspect_json(&el)),
        None => {
            // CDP fallback for web elements not in the AX tree
            if ghost_core::cdp::is_available().await {
                if let Ok(els) = ghost_core::cdp::find_elements(query).await {
                    if let Some(el) = els.first() {
                        return Ok(json!({
                            "role":       el.tag,
                            "title":      el.text,
                            "match_type": el.match_type,
                            "x":          el.center_x as i32,
                            "y":          el.center_y as i32,
                            "source":     "cdp",
                            "actionable": true,
                        }));
                    }
                }
            }
            Ok(json!({
                "found": false,
                "suggestion": "Element not found. Try ghost_find with a broader query or ghost_annotate."
            }))
        }
    }
}

pub async fn ghost_element_at(params: Value) -> Result<Value> {
    let x = params["x"]
        .as_f64()
        .ok_or_else(|| anyhow::anyhow!("x required"))? as i32;
    let y = params["y"]
        .as_f64()
        .ok_or_else(|| anyhow::anyhow!("y required"))? as i32;

    let ax = PlatformAXTree::new()?;
    let element = ax.element_at(x, y).await;

    match element {
        Some(el) => Ok(element_at_json(x, y, &el)),
        None => Ok(json!({ "x": x, "y": y, "element": null })),
    }
}

pub async fn ghost_screenshot(params: Value) -> Result<Value> {
    let full_resolution = bool_param(&params, "full_resolution", false);

    let frame = quick_screenshot(0)
        .await
        .map_err(|e| anyhow::anyhow!("Screenshot failed: {e}"))?;

    // Convert BGRA -> RGBA
    let mut rgba = vec![0u8; frame.data.len()];
    for i in (0..frame.data.len()).step_by(4) {
        rgba[i] = frame.data[i + 2];
        rgba[i + 1] = frame.data[i + 1];
        rgba[i + 2] = frame.data[i];
        rgba[i + 3] = frame.data[i + 3];
    }

    let (w, h) = (frame.width, frame.height);
    let img =
        image::RgbaImage::from_raw(w, h, rgba).ok_or_else(|| anyhow::anyhow!("Buffer mismatch"))?;

    // Optionally resize to max 1280 wide
    let out_img = if !full_resolution && w > 1280 {
        let scale = 1280.0 / w as f64;
        let new_w = 1280u32;
        let new_h = (h as f64 * scale) as u32;
        image::DynamicImage::ImageRgba8(img).resize(
            new_w,
            new_h,
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        image::DynamicImage::ImageRgba8(img)
    };

    let mut png_bytes = Vec::new();
    out_img.write_to(
        &mut std::io::Cursor::new(&mut png_bytes),
        image::ImageFormat::Png,
    )?;

    let b64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
    Ok(json!({
        "png_base64": b64,
        "width":  out_img.width(),
        "height": out_img.height(),
    }))
}

#[cfg(test)]
mod tests;
