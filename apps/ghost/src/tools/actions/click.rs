//! `ghost_click`: act on a ref, a query match, a CDP selector or raw coordinates.
//!
//! Refs and query matches run the semantic action first (`semantic`); `mode`
//! picks the chain: `hid` is HID only, `ax` semantic only, `auto` (default)
//! semantic then HID. HID is a fallback only for `press`, only after a semantic
//! ACTION_NOT_SUPPORTED/PLATFORM_NOT_SUPPORTED that delivered nothing, and only
//! when physical input is allowed; the result then says `via:"hid"` with a
//! `fallback_reason`. `ghost_hands::ax_press_at` is never used for these targets.

use anyhow::Result;
use serde_json::{json, Value};

use ghost_eyes::accessibility::find::{find_all, FindOptions};
use ghost_eyes::{
    AXTree, AXTreeNode, ElementAction, ElementIdentity, ElementLocator, GhostError, PlatformAXTree,
    SnapshotOptions, Verification, VerificationScope, WalkLimits, WindowTarget,
};
use ghost_hands::mode::plan_click_with;
use ghost_hands::{ClickMode, ClickPlan};

use super::pointer::click_at;
use super::semantic::{act, act_on, action_options, allows_hid_fallback, outcome_json};
use super::targets::{cdp_target, dom_target};
use super::{focus_requested_app, hid_allowed, hid_denied, hid_disposition};
use crate::refmap::center_of;
use crate::tools::refs::resolve_ref;
use crate::tools::{nonblank_str_param, required_query, str_param};

/// At most this many query matches are collected (and reported in `match_count`).
const MATCH_LIMIT: usize = 50;
/// At most this many candidates are listed in an AMBIGUOUS_TARGET error.
const CANDIDATE_LIMIT: usize = 10;

pub async fn ghost_click(params: Value) -> Result<Value> {
    let mode = ClickMode::parse(str_param(&params, "mode"));
    let action = parse_action(&params)?;
    let hid_ok = hid_allowed(&params);
    let reference = str_param(&params, "ref");
    let query = nonblank_str_param(&params, "query");

    // Refuse before focusing anything.
    if action != ElementAction::Press {
        if mode == ClickMode::Hid {
            return Err(GhostError::invalid_args(format!(
                "mode 'hid' can only press; action '{}' runs semantically (use mode 'auto' or 'ax')",
                action.name()
            ))
            .into());
        }
        if reference.is_none() && query.is_none() {
            return Err(GhostError::invalid_args(format!(
                "action '{}' needs a ref or query target",
                action.name()
            ))
            .into());
        }
    }
    if mode == ClickMode::Hid && !hid_ok {
        return Err(hid_denied().into());
    }

    focus_requested_app(str_param(&params, "app")).await;

    // Ref from a prior ghost_snapshot: strict resolution (STALE_REF/AMBIGUOUS_TARGET).
    if let Some(r) = reference {
        let resolved = resolve_ref(r, str_param(&params, "snapshot")).await?;
        let mut out = if mode == ClickMode::Hid {
            hid_click(&params, resolved.center, mode, Some(r), None).await?
        } else {
            match act(&resolved, &action, &action_options(true)).await {
                Ok(outcome) => with_point(outcome_json(&outcome), resolved.center),
                Err(e) => {
                    hid_fallback(&params, e, &action, mode, hid_ok, resolved.center, r).await?
                }
            }
        };
        out["ref"] = json!(r);
        out["method"] = json!("ref");
        if !resolved.qualified {
            out["ref_resolved_against"] = json!(resolved.snapshot_id);
        }
        return Ok(out);
    }

    // Raw coordinates: no element, so the legacy plan applies (AX press at the
    // point in `ax` mode, or in `auto` mode when physical input is not allowed).
    if let (Some(x), Some(y)) = (params["x"].as_f64(), params["y"].as_f64()) {
        let plan = plan_click_with(mode, false, true, hid_ok).map_err(|_| hid_denied())?;
        let via = click_at(&params, x as i32, y as i32, plan, None).await?;
        return Ok(json!({
            "success": true, "x": x, "y": y, "method": "coordinates", "via": via,
            "disposition": hid_disposition(),
        }));
    }

    // dom_id / dom_class: direct CDP lookup by CSS #id or .class selector.
    if action == ElementAction::Press {
        if let Some((el, selector, method)) = dom_target(&params).await {
            let plan = plan_click_with(mode, false, false, hid_ok).map_err(|_| hid_denied())?;
            let via = click_at(&params, el.x, el.y, plan, Some(&selector)).await?;
            return Ok(json!({
                "success": true, "x": el.x, "y": el.y, "element": el.text, "method": method,
                "via": via, "disposition": hid_disposition(),
            }));
        }
    }

    let query = required_query(&params, "ref, query, dom_id/dom_class, or x/y")?;
    query_click(&params, query, &action, mode, hid_ok).await
}

/// The `action` param (default `press`) with its optional `value`.
pub(super) fn parse_action(params: &Value) -> Result<ElementAction, GhostError> {
    let name = str_param(params, "action").map_or("press", str::trim);
    ElementAction::parse(name, str_param(params, "value"))
}

/// Click the element a query names in the focused window.
async fn query_click(
    params: &Value,
    query: &str,
    action: &ElementAction,
    mode: ClickMode,
    hid_ok: bool,
) -> Result<Value> {
    let ax = PlatformAXTree::new()?;
    let captured = ax
        .snapshot(SnapshotOptions {
            target: WindowTarget::Focused,
            limits: WalkLimits::platform_default(),
            activate_renderer: true,
        })
        .await;
    let captured = match captured {
        Ok(captured) => Some(captured),
        // A press can still reach the element through CDP below.
        Err(_) if *action == ElementAction::Press => None,
        Err(e) => return Err(e.into()),
    };
    let picked = match &captured {
        Some(snap) => pick_query_match(&snap.root, query, action)?.map(|m| (snap, m)),
        None => None,
    };
    if let Some((snap, m)) = picked {
        let locator = query_locator(snap.pid, snap.window_title.as_deref(), &m);
        let center = center_of(m.node);
        let mut out = if mode == ClickMode::Hid {
            hid_click(params, center, mode, Some(query), None).await?
        } else {
            let opts = action_options(true);
            match act_on(&locator, center, action, &opts, "ghost_click").await {
                Ok(outcome) => with_point(outcome_json(&outcome), center),
                Err(e) => hid_fallback(params, e, action, mode, hid_ok, center, query).await?,
            }
        };
        out["element"] = json!(m.node.title);
        out["method"] = json!("ax_query");
        out["match_count"] = json!(m.match_count);
        return Ok(out);
    }

    if *action != ElementAction::Press {
        return Err(not_found(query).into());
    }
    // CDP fallback: Chrome elements not exposed in the AX tree (iframes, SPAs, ...).
    // The click itself is physical input.
    if let Some(el) = cdp_target(query).await {
        cdp_click_allowed(mode)?;
        if !hid_ok {
            return Err(hid_denied().into());
        }
        let mut out = hid_click(params, Some((el.x, el.y)), mode, Some(query), None).await?;
        out["element"] = json!(el.text);
        out["method"] = json!("cdp");
        return Ok(out);
    }
    Err(not_found(query).into())
}

/// A CDP match is clicked with physical input, which `mode:"ax"` (semantic only)
/// never allows.
pub(super) fn cdp_click_allowed(mode: ClickMode) -> Result<(), GhostError> {
    if mode != ClickMode::Ax {
        return Ok(());
    }
    Err(GhostError::not_supported(
        "the element is reachable only through the browser (CDP), which needs a physical click; mode \"ax\" is semantic only",
    )
    .with_details(json!({ "reason": "cdp_needs_hid" }))
    .with_suggestion("use mode \"auto\" or \"hid\" to allow a physical click"))
}

fn not_found(query: &str) -> GhostError {
    GhostError::not_found(format!("could not find an element matching '{query}'"))
        .with_suggestion("try ghost_snapshot and click a ref, ghost_annotate for numbered labels, or x/y coordinates")
}

/// A query match: the first match in walker order, and how many matched.
pub(super) struct QueryMatch<'a> {
    pub(super) path: Vec<u32>,
    pub(super) node: &'a AXTreeNode,
    pub(super) match_count: usize,
}

/// Pick the element a query acts on. `press` keeps the legacy first-match choice;
/// every other action needs a unique match (else AMBIGUOUS_TARGET). Matching never
/// compares a secure node's value.
pub(super) fn pick_query_match<'a>(
    root: &'a AXTreeNode,
    query: &str,
    action: &ElementAction,
) -> Result<Option<QueryMatch<'a>>, GhostError> {
    let opts = FindOptions {
        match_identifier: !cfg!(target_os = "macos"),
        match_value: true,
    };
    let matches = find_all(root, query, opts, MATCH_LIMIT);
    let match_count = matches.len();
    if match_count > 1 && *action != ElementAction::Press {
        let candidates: Vec<Value> = matches
            .iter()
            .take(CANDIDATE_LIMIT)
            .map(|(path, node)| {
                json!({ "role": node.stable_role(), "name": node.title, "path": path, "bounds": node.bounds })
            })
            .collect();
        return Err(GhostError::ambiguous(
            format!(
                "{match_count} elements match '{query}'; action '{}' needs exactly one",
                action.name()
            ),
            Value::Array(candidates),
        )
        .with_suggestion("narrow the query, or ghost_snapshot and act on a ref"));
    }
    Ok(matches.into_iter().next().map(|(path, node)| QueryMatch {
        path,
        node,
        match_count,
    }))
}

/// The locator for a query match in the focused window's snapshot.
fn query_locator(pid: i32, title: Option<&str>, m: &QueryMatch<'_>) -> ElementLocator {
    let title = title.map(str::trim).filter(|t| !t.is_empty());
    let window = match (pid > 0, title) {
        (true, Some(title)) => WindowTarget::PidTitle {
            pid,
            title: title.to_string(),
        },
        (true, None) => WindowTarget::Pid { pid },
        (false, _) => WindowTarget::Focused,
    };
    ElementLocator {
        pid,
        window,
        path: m.path.clone(),
        identity: ElementIdentity::from_node(m.node),
    }
}

/// After a semantic failure: HID-press when the rules allow it, POLICY_DENIED when
/// HID would be needed but is not allowed, else the semantic error.
async fn hid_fallback(
    params: &Value,
    err: GhostError,
    action: &ElementAction,
    mode: ClickMode,
    hid_ok: bool,
    center: Option<(i32, i32)>,
    target: &str,
) -> Result<Value> {
    let eligible =
        mode == ClickMode::Auto && *action == ElementAction::Press && allows_hid_fallback(&err);
    if !eligible || center.is_none() {
        return Err(err.into());
    }
    if !hid_ok {
        return Err(hid_denied()
            .with_details(json!({ "semantic_error": err.code.as_str() }))
            .into());
    }
    hid_click(params, center, mode, Some(target), Some(err.code.as_str())).await
}

/// The plan for an HID press on an element target: never an AX press at the point.
pub(super) const fn hid_fallback_plan(mode: ClickMode) -> ClickPlan {
    match mode {
        ClickMode::Hid => ClickPlan::HidPlain,
        ClickMode::Auto | ClickMode::Ax => ClickPlan::HidRestore,
    }
}

/// HID-press an element target at its centre (`via:"hid"`).
async fn hid_click(
    params: &Value,
    center: Option<(i32, i32)>,
    mode: ClickMode,
    target: Option<&str>,
    fallback_reason: Option<&str>,
) -> Result<Value> {
    let (x, y) = center.ok_or_else(|| {
        GhostError::not_supported("the element has no bounds to click")
            .with_suggestion("run ghost_snapshot again, or click by x/y")
    })?;
    click_at(params, x, y, hid_fallback_plan(mode), target).await?;
    let verification = Verification {
        scope: VerificationScope::Unavailable,
        reason: Some("insufficient_evidence"),
    };
    let mut out = json!({
        "success": true, "x": x, "y": y, "via": "hid",
        "disposition": hid_disposition(), "verification": verification,
    });
    if let Some(reason) = fallback_reason {
        out["fallback_reason"] = json!(reason);
    }
    Ok(out)
}

/// Add `success`, `x` and `y` (the element centre, or null) to a semantic result.
fn with_point(mut out: Value, center: Option<(i32, i32)>) -> Value {
    out["success"] = json!(true);
    out["x"] = json!(center.map(|c| c.0));
    out["y"] = json!(center.map(|c| c.1));
    out
}
