//! Snapshot building, rendering and re-identification tests.

mod refs_test;
mod store_test;

use super::*;
use ghost_eyes::{AXTreeNode, Bounds};
use serde_json::json;

fn node(
    role: &str,
    title: Option<&str>,
    bounds: Option<Bounds>,
    children: Vec<AXTreeNode>,
) -> AXTreeNode {
    AXTreeNode {
        role: role.into(),
        title: title.map(|s| s.into()),
        value: None,
        identifier: None,
        bounds,
        children,
        enabled: true,
        focused: false,
        hidden: false,
        ..Default::default()
    }
}

fn bounds(x: i32, y: i32, w: u32, h: u32) -> Bounds {
    Bounds {
        x,
        y,
        width: w,
        height: h,
    }
}

fn sample() -> AXTreeNode {
    node(
        "window",
        Some("Main"),
        Some(bounds(0, 0, 800, 600)),
        vec![
            node(
                "group",
                None,
                None,
                vec![
                    node("button", Some("Send"), Some(bounds(10, 20, 80, 30)), vec![]),
                    node(
                        "button",
                        Some("Cancel"),
                        Some(bounds(100, 20, 80, 30)),
                        vec![],
                    ),
                ],
            ),
            node(
                "textfield",
                Some("Search"),
                Some(bounds(10, 100, 200, 24)),
                vec![],
            ),
        ],
    )
}

#[test]
fn refs_assigned_in_dfs_preorder() {
    let snap = Snapshot::build(&sample(), Some("App".into()), 42, 1000);
    // window(named+children)=@e1, button Send=@e2, button Cancel=@e3, textfield=@e4.
    // The unnamed "group" gets no ref.
    assert_eq!(snap.tree.ref_id.as_deref(), Some("@e1"));
    assert_eq!(snap.ref_count(), 4);
    let send = snap.find_ref("@e2").expect("@e2");
    assert_eq!(send.name.as_deref(), Some("Send"));
    let field = snap.find_ref("@e4").expect("@e4");
    assert_eq!(field.role, "textfield");
}

#[test]
fn render_prunes_depth_but_reports_children_count() {
    let snap = Snapshot::build(&sample(), None, 1, 1000);
    let shallow = render(&snap.tree, 0);
    assert_eq!(shallow["children_count"], json!(2));
    assert!(
        shallow.get("children").is_none(),
        "depth 0 must not emit children"
    );
    let deep = render(&snap.tree, 3);
    assert!(deep["children"].is_array());
}

#[test]
fn reidentify_prefers_exact_bounds_then_falls_back() {
    let snap = Snapshot::build(&sample(), None, 1, 1000);
    let send = snap.find_ref("@e2").unwrap();

    // Unchanged tree → exact bounds match → center of (10,20,80,30) = (50,35).
    match reidentify(&sample(), send) {
        Resolved::At { x, y } => {
            assert_eq!((x, y), (50, 35));
        }
        Resolved::Stale(e) => panic!("unexpected stale: {e}"),
    }

    // Moved button (same role+name, different bounds) → still re-identified at new center.
    let moved = node(
        "window",
        Some("Main"),
        None,
        vec![node(
            "button",
            Some("Send"),
            Some(bounds(200, 200, 80, 30)),
            vec![],
        )],
    );
    match reidentify(&moved, send) {
        Resolved::At { x, y } => assert_eq!((x, y), (240, 215)),
        Resolved::Stale(e) => panic!("unexpected stale: {e}"),
    }
}

#[test]
fn reidentify_stale_when_absent() {
    let snap = Snapshot::build(&sample(), None, 1, 1000);
    let send = snap.find_ref("@e2").unwrap();
    let other = node(
        "window",
        Some("Other"),
        None,
        vec![node(
            "button",
            Some("Quit"),
            Some(bounds(0, 0, 10, 10)),
            vec![],
        )],
    );
    assert!(matches!(reidentify(&other, send), Resolved::Stale(_)));
}
