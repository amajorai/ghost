//! Tree-helper tests for the perception tools.

use super::redact;
use super::walk::*;
use super::*;
use ghost_eyes::{AXTreeNode, Bounds};

fn node(
    role: &str,
    title: Option<&str>,
    value: Option<&str>,
    children: Vec<AXTreeNode>,
) -> AXTreeNode {
    AXTreeNode {
        role: role.into(),
        title: title.map(Into::into),
        value: value.map(Into::into),
        identifier: None,
        bounds: Some(Bounds {
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        }),
        children,
        enabled: true,
        focused: false,
        hidden: false,
        ..Default::default()
    }
}

fn with_ident(mut n: AXTreeNode, ident: &str) -> AXTreeNode {
    n.identifier = Some(ident.into());
    n
}

#[test]
fn is_actionable_matches_interactive_roles_case_insensitively() {
    for role in [
        "AXButton",
        "button",
        "AXLink",
        "textfield",
        "AXTextField",
        "checkbox",
        "combo box",
        "slider",
        "AXTab",
        "menu item",
        "cell",
        "row",
    ] {
        assert!(
            is_actionable(&node(role, None, None, vec![])),
            "{role} should be actionable"
        );
    }
}

#[test]
fn is_actionable_rejects_non_interactive_roles() {
    for role in ["AXGroup", "window", "static text", "image", ""] {
        assert!(
            !is_actionable(&node(role, None, None, vec![])),
            "{role} should not be actionable"
        );
    }
}

#[test]
fn is_editable_matches_text_field_edit_combo() {
    assert!(is_editable(&node("AXTextField", None, None, vec![])));
    assert!(is_editable(&node("text area", None, None, vec![])));
    assert!(is_editable(&node("search field", None, None, vec![])));
    assert!(is_editable(&node("AXComboBox", None, None, vec![])));
    assert!(!is_editable(&node("AXButton", None, None, vec![])));
}

#[test]
fn is_layout_container_covers_structural_roles_and_empty() {
    for role in [
        "group",
        "AXGroup",
        "scrollarea",
        "box",
        "generic",
        "none",
        "",
    ] {
        assert!(
            is_layout_container(&node(role, None, None, vec![])),
            "{role} should be a layout container"
        );
    }
    assert!(!is_layout_container(&node("AXButton", None, None, vec![])));
}

#[test]
fn collect_interactive_tunnels_through_layout_containers_without_spending_depth() {
    // A button nested under many AXGroups (which cost 0 depth) is still found at
    // max_depth 1, whereas a semantic wrapper would consume the budget.
    let deep = node(
        "AXGroup",
        None,
        None,
        vec![node(
            "AXGroup",
            None,
            None,
            vec![node(
                "AXGroup",
                None,
                None,
                vec![node("AXButton", Some("Deep"), None, vec![])],
            )],
        )],
    );
    let found = collect_interactive(&deep, 0, 1);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0]["title"], json!("Deep"));
    assert_eq!(found[0]["actionable"], json!(true));
}

#[test]
fn collect_interactive_respects_depth_for_semantic_nodes() {
    // A semantic (non-layout) wrapper consumes depth budget, so at max_depth 0 the
    // nested button is never reached.
    let tree = node(
        "window",
        Some("Main"),
        None,
        vec![node("AXButton", Some("Child"), None, vec![])],
    );
    assert!(collect_interactive(&tree, 0, 0).is_empty());
    assert_eq!(collect_interactive(&tree, 0, 1).len(), 1);
}

#[test]
fn find_elements_filters_by_query_role_and_identifier() {
    let tree = node(
        "window",
        Some("Main"),
        None,
        vec![
            node("AXButton", Some("Send Message"), None, vec![]),
            node("AXButton", Some("Cancel"), None, vec![]),
            with_ident(node("AXTextField", Some("To"), None, vec![]), "compose-to"),
        ],
    );

    // Query matches title substring (case-insensitive).
    let mut out = vec![];
    find_elements(&tree, "send", "", "", "", 25, &mut out);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["title"], json!("Send Message"));

    // Role filter narrows to text fields only.
    let mut out = vec![];
    find_elements(&tree, "", "textfield", "", "", 25, &mut out);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["role"], json!("AXTextField"));

    // Identifier filter matches the compose-to field.
    let mut out = vec![];
    find_elements(&tree, "", "", "compose-to", "", 25, &mut out);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["title"], json!("To"));
}

#[test]
fn find_elements_empty_query_matches_everything_within_depth() {
    let tree = node(
        "window",
        Some("W"),
        None,
        vec![node("AXButton", Some("B"), None, vec![])],
    );
    let mut out = vec![];
    find_elements(&tree, "", "", "", "", 25, &mut out);
    // window + button
    assert_eq!(out.len(), 2);
}

#[test]
fn find_node_ref_returns_first_matching_subtree() {
    let tree = node(
        "window",
        Some("Main"),
        None,
        vec![
            node("AXStaticText", Some("Hello"), None, vec![]),
            node("AXButton", Some("Submit"), Some("clickme"), vec![]),
        ],
    );
    let found = find_node_ref(&tree, "submit").expect("found submit");
    assert_eq!(found.title.as_deref(), Some("Submit"));
    // Value substring also matches.
    let by_value = find_node_ref(&tree, "clickme").expect("found by value");
    assert_eq!(by_value.title.as_deref(), Some("Submit"));
    assert!(find_node_ref(&tree, "nonexistent").is_none());
}

#[test]
fn collect_text_dedups_titles_and_trims_within_depth() {
    let tree = node(
        "window",
        Some("Title"),
        Some("  spaced value  "),
        vec![
            node("AXStaticText", Some("Title"), None, vec![]), // duplicate title
            node("AXStaticText", Some("Unique"), None, vec![]),
        ],
    );
    let mut out = vec![];
    collect_text(&tree, 25, 0, &mut out);
    assert!(out.contains(&"spaced value".to_string()));
    assert!(out.contains(&"Title".to_string()));
    assert!(out.contains(&"Unique".to_string()));
    // "Title" appears once despite two nodes carrying it.
    assert_eq!(out.iter().filter(|t| *t == "Title").count(), 1);
}

#[test]
fn collect_text_stops_at_max_depth() {
    let tree = node(
        "window",
        None,
        Some("root"),
        vec![node("group", None, Some("child"), vec![])],
    );
    let mut out = vec![];
    collect_text(&tree, 0, 0, &mut out);
    assert_eq!(out, vec!["root".to_string()]);
}

#[test]
fn node_to_value_and_focused_element_carry_expected_fields() {
    let n = node("AXButton", Some("Go"), Some("v"), vec![]);
    let v = node_to_value(&n);
    assert_eq!(v["role"], json!("AXButton"));
    assert_eq!(v["title"], json!("Go"));
    assert_eq!(v["actionable"], json!(true));

    let fe = focused_element(&node("AXTextField", Some("Email"), Some("a@b"), vec![]));
    assert_eq!(fe["role"], json!("AXTextField"));
    assert_eq!(fe["editable"], json!(true));
}

// ── Secure-value redaction ──────────────────────────────────────────────────

const SECRET: &str = "hunter2-secret";

/// A password field whose backend (wrongly) still carries the value, so the
/// consumer-side guard is exercised, not just the walker's redaction.
fn secure_field(focused: bool) -> AXTreeNode {
    let mut n = node("AXSecureTextField", Some("Password"), Some(SECRET), vec![]);
    n.secure = true;
    n.focused = focused;
    n
}

fn login_tree(focused: bool) -> AXTreeNode {
    node(
        "window",
        Some("Login"),
        None,
        vec![
            node("AXTextField", Some("User"), Some("alice"), vec![]),
            secure_field(focused),
            node("AXButton", Some("Sign in"), None, vec![]),
        ],
    )
}

fn assert_no_secret(v: &Value) {
    let text = v.to_string();
    assert!(!text.contains(SECRET), "secret leaked: {text}");
}

#[test]
fn node_json_withholds_secure_value_and_marks_it() {
    let v = redact::node_json(&secure_field(false));
    assert_eq!(v["value"], Value::Null);
    assert_eq!(v["secure"], json!(true));
    assert_eq!(v["value_withheld"], json!(true));
    assert_no_secret(&v);

    let plain = redact::node_json(&node("AXTextField", Some("User"), Some("alice"), vec![]));
    assert_eq!(plain["value"], json!("alice"));
    assert!(plain.get("secure").is_none());
    assert!(plain.get("value_withheld").is_none());
}

#[test]
fn node_json_treats_secure_states_as_secure() {
    let mut n = node("AXTextField", None, Some(SECRET), vec![]);
    n.states = Some(ghost_eyes::ElementState {
        secure: true,
        ..Default::default()
    });
    let v = redact::node_json(&n);
    assert_eq!(v["value_withheld"], json!(true));
    assert_no_secret(&v);
}

#[test]
fn node_json_with_cannot_reintroduce_a_value() {
    let v = redact::node_json_with(&secure_field(false), json!({ "value": SECRET, "x": 1 }));
    assert_eq!(v["value"], Value::Null);
    assert_eq!(v["x"], json!(1));
    assert_no_secret(&v);
}

#[test]
fn read_never_outputs_a_secure_value() {
    let tree = login_tree(false);
    let lines = read_lines(&tree, "", 25);
    assert!(lines.contains(&"alice".to_string()));
    assert!(lines.contains(&"Password".to_string()));
    assert!(lines.iter().all(|l| !l.contains(SECRET)), "{lines:?}");
    // A query equal to the secret selects no subtree.
    assert!(read_lines(&tree, SECRET, 25).is_empty());
}

#[test]
fn find_never_matches_or_outputs_a_secure_value() {
    let tree = login_tree(false);
    let mut out = vec![];
    find_elements(&tree, SECRET, "", "", "", 25, &mut out);
    assert!(out.is_empty(), "query equal to the secret matched: {out:?}");
    let mut out = vec![];
    find_elements(&tree, "hunter2", "", "", "", 25, &mut out);
    assert!(out.is_empty());
    assert!(find_node_ref(&tree, SECRET).is_none());

    // Matching by title still finds the field, without its value.
    let mut out = vec![];
    find_elements(&tree, "password", "", "", "", 25, &mut out);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["value_withheld"], json!(true));
    for v in &out {
        assert_no_secret(v);
    }

    // An unfiltered listing includes the field but never its value.
    let mut all = vec![];
    find_elements(&tree, "", "", "", "", 25, &mut all);
    assert_eq!(all.len(), 4);
    assert_no_secret(&Value::Array(all));
}

#[test]
fn inspect_element_at_context_never_output_a_secure_value() {
    let field = secure_field(true);
    let inspected = inspect_json(&field);
    assert_eq!(inspected["value_withheld"], json!(true));
    assert_eq!(inspected["editable"], json!(true));
    assert_no_secret(&inspected);

    let at = element_at_json(5, 6, &field);
    assert_eq!(at["x"], json!(5));
    assert_eq!(at["value_withheld"], json!(true));
    assert_no_secret(&at);

    let tree = login_tree(true);
    let focused = focused_element(&tree);
    assert_eq!(focused["title"], json!("Password"));
    assert_eq!(focused["value_withheld"], json!(true));
    assert_no_secret(&focused);
    assert_no_secret(&Value::Array(collect_interactive(&tree, 0, 3)));
}

#[test]
fn focused_element_prefers_the_focused_node_and_falls_back_to_root() {
    let mut tree = login_tree(false);
    assert_eq!(focused_element(&tree)["title"], json!("Login"));
    tree.children[0].focused = true;
    let fe = focused_element(&tree);
    assert_eq!(fe["title"], json!("User"));
    assert_eq!(fe["focused"], json!(true));
}

#[test]
fn is_actionable_uses_canonical_role_keys() {
    let mut n = node("Schaltfläche", None, None, vec![]);
    assert!(!is_actionable(&n));
    n.role_key = Some("button".into());
    assert!(is_actionable(&n));
    assert!(is_actionable(&node("push button", None, None, vec![])));
    assert!(is_actionable(&node("AXRadioButton", None, None, vec![])));
}

#[test]
fn find_respects_tunnelled_depth_for_queries() {
    let tree = node(
        "window",
        None,
        None,
        vec![node(
            "AXGroup",
            None,
            None,
            vec![node(
                "toolbar",
                None,
                None,
                vec![node("AXButton", Some("Deep"), None, vec![])],
            )],
        )],
    );
    let mut out = vec![];
    find_elements(&tree, "deep", "", "", "", 1, &mut out);
    assert!(out.is_empty());
    find_elements(&tree, "deep", "", "", "", 2, &mut out);
    assert_eq!(out.len(), 1);
}
