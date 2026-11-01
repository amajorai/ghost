//! Strict refs: ref parsing, legacy snapshot loading, secure redaction, strict
//! resolution and the bare-ref caller check.

use super::{bounds, node, sample};
use crate::refmap::{render, resolve_entry, Resolved, SkeletonNode, Snapshot};
use crate::tools::refs::{
    check_bare_ref_caller, locator_for, parse_ref, parse_snapshot_param, pinned_snapshot,
};
use ghost_eyes::{AXTreeNode, ErrorCode, MatchBy, WindowTarget};
use serde_json::json;

const SID: &str = "s1700000000000-4242000007";

#[test]
fn parse_ref_accepts_bare_and_qualified_forms() {
    let bare = parse_ref("@e3").expect("bare");
    assert_eq!(bare.snapshot, None);
    assert_eq!(bare.local, "@e3");

    // The sid itself contains '-', and the split is on the last ':'.
    let qualified = parse_ref(&format!("@{SID}:e3")).expect("qualified");
    assert_eq!(qualified.snapshot.as_deref(), Some(SID));
    assert_eq!(qualified.local, "@e3");

    assert_eq!(parse_ref("e12").expect("no @").local, "@e12");
    assert_eq!(
        parse_ref(&format!("@e{}", u32::MAX)).expect("max").local,
        format!("@e{}", u32::MAX)
    );
}

#[test]
fn parse_ref_rejects_malformed_input() {
    for bad in [
        "",
        "@",
        "@e",
        "@e0",
        "@e01",
        "@e-1",
        "@e+1",
        "@x3",
        "@E3",
        "@e3x",
        "@:e3",
        "@bogus:e3",
        "@s1-:e3",
        "@e4294967296",
        "@s1-2:e3:e4",
    ] {
        let err = parse_ref(bad).expect_err(bad);
        assert_eq!(err.code, ErrorCode::InvalidArgs, "{bad}");
    }
}

#[test]
fn qualified_sid_must_agree_with_snapshot_param() {
    let qualified = parse_ref(&format!("@{SID}:e3")).unwrap();
    assert_eq!(
        pinned_snapshot(&qualified, Some(SID)).unwrap().as_deref(),
        Some(SID)
    );
    let err = pinned_snapshot(&qualified, Some("s1-2")).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgs);

    let bare = parse_ref("@e3").unwrap();
    assert_eq!(pinned_snapshot(&bare, None).unwrap(), None);
    assert_eq!(
        pinned_snapshot(&bare, Some(&format!("@{SID}:")))
            .unwrap()
            .as_deref(),
        Some(SID)
    );
    assert_eq!(
        parse_snapshot_param("../etc").unwrap_err().code,
        ErrorCode::InvalidArgs
    );
}

#[test]
fn bare_ref_from_another_identified_caller_is_refused() {
    let err = check_bare_ref_caller(Some("@ryu/codex"), Some("@ryu/claude"), SID).unwrap_err();
    assert_eq!(err.code, ErrorCode::StaleRef);
    assert_eq!(
        err.details.as_ref().unwrap()["reason"],
        "bare_ref_other_caller"
    );
    assert!(err
        .suggestion
        .as_deref()
        .unwrap()
        .contains("@<snapshot_id>:eN"));

    assert!(check_bare_ref_caller(Some("@ryu/codex"), Some("@ryu/codex"), SID).is_ok());
    assert!(check_bare_ref_caller(None, Some("@ryu/codex"), SID).is_ok());
    assert!(check_bare_ref_caller(Some("@ryu/codex"), None, SID).is_ok());
    assert!(check_bare_ref_caller(None, None, SID).is_ok());
}

#[test]
fn legacy_sealed_json_without_new_fields_loads() {
    let legacy = json!({
        "id": SID,
        "app": "Finder",
        "pid": 7,
        "created_ms": 1000,
        "tree": {
            "ref": "@e1", "role": "window", "name": "Main",
            "children": [
                { "ref": "@e2", "role": "push button", "name": "OK", "children": [] }
            ]
        }
    });
    let snap: Snapshot = serde_json::from_value(legacy).expect("legacy snapshot");
    assert!(!snap.complete);
    assert_eq!(snap.truncated_nodes, 0);
    assert_eq!(snap.creator, None);
    let ok = snap.find_ref("@e2").unwrap();
    assert!(ok.path.is_empty());
    assert!(ok.enabled);
    assert!(!ok.secure && !ok.subtree_truncated);
    assert_eq!(ok.role_key, None);
}

#[test]
fn new_fields_round_trip_and_paths_follow_walker_order() {
    let snap = Snapshot::build(&sample(), None, 1, 1000);
    assert_eq!(snap.find_ref("@e3").unwrap().path, vec![0, 1]);
    assert_eq!(snap.find_ref("@e4").unwrap().path, vec![1]);
    let back: Snapshot = serde_json::from_value(serde_json::to_value(&snap).unwrap()).unwrap();
    assert_eq!(back.find_ref("@e3").unwrap().path, vec![0, 1]);
}

fn secure_field() -> AXTreeNode {
    AXTreeNode {
        value: Some("hunter2-secret".into()),
        secure: true,
        ..node(
            "textfield",
            Some("Password"),
            Some(bounds(0, 0, 10, 10)),
            vec![],
        )
    }
}

#[test]
fn secure_value_is_neither_stored_nor_rendered() {
    let root = node("window", Some("Login"), None, vec![secure_field()]);
    let snap = Snapshot::build(&root, None, 1, 1000);
    let field = snap.find_ref("@e2").expect("password field ref");
    assert!(field.secure);
    assert_eq!(field.value, None);

    let stored = serde_json::to_string(&snap).unwrap();
    assert!(!stored.contains("hunter2"));
    let rendered = render(&snap.tree, 3);
    assert!(!rendered.to_string().contains("hunter2"));
    let child = &rendered["children"][0];
    assert_eq!(child["secure"], json!(true));
    assert_eq!(child["value_withheld"], json!(true));
    assert!(child.get("value").is_none());
}

#[test]
fn render_marks_truncated_subtrees() {
    let mut cut = node("group", Some("Lazy list"), None, vec![]);
    cut.subtree_truncated = true;
    let root = node("window", Some("Main"), None, vec![cut]);
    let snap = Snapshot::build(&root, None, 1, 1000);
    let rendered = render(&snap.tree, 3);
    assert_eq!(rendered["children"][0]["subtree_truncated"], json!(true));
    assert!(rendered.get("subtree_truncated").is_none());
}

#[test]
fn ambiguous_resolution_is_refused() {
    let snap = Snapshot::build(&sample(), None, 1, 1000);
    let send = snap.find_ref("@e2").unwrap();
    // The stored path no longer holds Send, and two moved Send buttons now exist.
    let twins = node(
        "window",
        Some("Main"),
        None,
        vec![
            node("button", Some("Send"), Some(bounds(300, 0, 80, 30)), vec![]),
            node("button", Some("Send"), Some(bounds(400, 0, 80, 30)), vec![]),
        ],
    );
    let err = resolve_entry(&twins, send).unwrap_err();
    assert_eq!(err.code, ErrorCode::AmbiguousTarget);
    assert_eq!(err.details.as_ref().unwrap()["candidate_count"], json!(2));
    match crate::refmap::reidentify(&twins, send) {
        Resolved::Stale(msg) => assert!(msg.starts_with("AMBIGUOUS_TARGET:"), "{msg}"),
        Resolved::At { .. } => panic!("ambiguous target must not resolve"),
    }
}

#[test]
fn resolution_uses_path_then_identity() {
    let snap = Snapshot::build(&sample(), None, 1, 1000);
    let cancel = snap.find_ref("@e3").unwrap();
    let (found, path, by) = resolve_entry(&sample(), cancel).unwrap();
    assert_eq!(by, MatchBy::Path);
    assert_eq!(path, vec![0, 1]);
    assert_eq!(found.title.as_deref(), Some("Cancel"));

    // Same element, renamed: identity drift is refused, never guessed.
    let renamed = node(
        "window",
        Some("Main"),
        None,
        vec![node(
            "group",
            None,
            None,
            vec![
                node("button", Some("Send"), None, vec![]),
                node("button", Some("Abort"), None, vec![]),
            ],
        )],
    );
    let err = resolve_entry(&renamed, cancel).unwrap_err();
    assert_eq!(err.code, ErrorCode::StaleRef);
}

#[test]
fn legacy_pushbutton_entry_resolves() {
    let entry = SkeletonNode {
        ref_id: Some("@e2".into()),
        role: "push button".into(),
        name: Some("OK".into()),
        value: None,
        identifier: None,
        bounds: None,
        children: vec![],
        path: vec![],
        role_key: None,
        native_id: None,
        enabled: true,
        secure: false,
        subtree_truncated: false,
    };
    let mut ok = node(
        "push button",
        Some("OK"),
        Some(bounds(5, 5, 10, 10)),
        vec![],
    );
    ok.role_key = Some("button".into());
    let live = node("frame", Some("Dialog"), None, vec![ok]);
    let (found, path, by) = resolve_entry(&live, &entry).expect("legacy pushbutton resolves");
    assert_eq!(found.title.as_deref(), Some("OK"));
    assert_eq!(path, vec![0]);
    assert_eq!(by, MatchBy::RoleName);
}

#[test]
fn locator_pins_pid_and_window_title() {
    let mut snap = Snapshot::build(&sample(), None, 42, 1000);
    let entry = snap.find_ref("@e2").unwrap().clone();
    let loc = locator_for(&snap, &entry);
    assert_eq!(loc.pid, 42);
    assert_eq!(
        loc.window,
        WindowTarget::PidTitle {
            pid: 42,
            title: "Main".into()
        }
    );
    assert_eq!(loc.path, vec![0, 0]);
    snap.window_title = None;
    assert_eq!(
        locator_for(&snap, &entry).window,
        WindowTarget::Pid { pid: 42 }
    );
    snap.pid = 0;
    assert_eq!(locator_for(&snap, &entry).window, WindowTarget::Focused);
}
