//! Target resolution shared by the action tools: accessibility queries and
//! Chrome DevTools (CDP) selectors resolved to screen points.

use anyhow::Result;
use serde_json::Value;

use ghost_eyes::{AXTree, Bounds, PlatformAXTree};

use crate::tools::str_param;

/// Screen-space center of an accessibility element's bounds.
pub(super) fn center(bounds: &Bounds) -> (i32, i32) {
    (
        bounds.x + bounds.width as i32 / 2,
        bounds.y + bounds.height as i32 / 2,
    )
}

/// Center and title of the accessibility element matching `query`, or `None`
/// when nothing matches or the match has no bounds.
pub(super) async fn ax_target(query: &str) -> Result<Option<(i32, i32, Option<String>)>> {
    let ax = PlatformAXTree::new()?;
    Ok(ax.find_element(query).await.and_then(|el| {
        let (x, y) = center(el.bounds.as_ref()?);
        Some((x, y, el.title))
    }))
}

/// A Chrome element resolved through CDP to screen-absolute coordinates.
pub(super) struct CdpTarget {
    pub(super) x: i32,
    pub(super) y: i32,
    pub(super) text: String,
}

/// First Chrome element matching a CSS selector or query, or `None` when CDP is
/// unavailable or nothing matches.
pub(super) async fn cdp_target(query: &str) -> Option<CdpTarget> {
    if !ghost_core::cdp::is_available().await {
        return None;
    }
    let el = ghost_core::cdp::find_elements(query)
        .await
        .ok()?
        .into_iter()
        .next()?;
    let (win_x, win_y) = chrome_window_origin();
    let (x, y) = ghost_core::cdp::viewport_to_screen(el.center_x, el.center_y, win_x, win_y);
    Some(CdpTarget {
        x,
        y,
        text: el.text,
    })
}

/// The `dom_id` (`#id`) then `dom_class` (`.class`) CSS selectors a call names,
/// each with the `method` a match reports.
pub(super) fn dom_selectors(params: &Value) -> impl Iterator<Item = (String, &'static str)> + '_ {
    [
        ("dom_id", "#", "cdp_dom_id"),
        ("dom_class", ".", "cdp_dom_class"),
    ]
    .into_iter()
    .filter_map(move |(key, prefix, method)| {
        str_param(params, key).map(|value| (format!("{prefix}{value}"), method))
    })
}

/// The first `dom_selectors` element Chrome resolves, with its selector and method.
pub(super) async fn dom_target(params: &Value) -> Option<(CdpTarget, String, &'static str)> {
    for (selector, method) in dom_selectors(params) {
        if let Some(el) = cdp_target(&selector).await {
            return Some((el, selector, method));
        }
    }
    None
}

/// Return the screen-space top-left origin of the Chrome window.
/// Used to convert CDP viewport-relative coordinates to screen-absolute.
fn chrome_window_origin() -> (i32, i32) {
    #[cfg(target_os = "windows")]
    {
        use windows::core::w;
        use windows::Win32::Foundation::RECT;
        use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowRect};
        unsafe {
            if let Ok(hwnd) = FindWindowW(w!("Chrome_WidgetWin_1"), windows::core::PCWSTR::null()) {
                if !hwnd.0.is_null() {
                    let mut r = RECT::default();
                    let _ = GetWindowRect(hwnd, &mut r);
                    return (r.left, r.top);
                }
            }
        }
    }
    (0, 0)
}
