//! Tool-definition shape tests.

use super::*;
use std::collections::HashSet;

#[test]
fn there_are_thirty_one_tool_definitions() {
    assert_eq!(definitions().len(), 31);
}

#[test]
fn every_tool_name_is_unique() {
    let defs = definitions();
    let names: HashSet<&str> = defs
        .iter()
        .map(|d| d["name"].as_str().expect("name is a string"))
        .collect();
    assert_eq!(names.len(), defs.len(), "duplicate tool name detected");
}

#[test]
fn every_tool_is_well_formed() {
    for d in definitions() {
        let name = d["name"].as_str().expect("name");
        assert!(name.starts_with("ghost_"), "bad name: {name}");
        assert!(
            d["description"].as_str().is_some_and(|s| !s.is_empty()),
            "{name}: description must be a non-empty string"
        );
        let schema = &d["inputSchema"];
        assert_eq!(
            schema["type"], "object",
            "{name}: inputSchema.type must be object"
        );
        assert!(
            schema["properties"].is_object(),
            "{name}: inputSchema.properties must be an object"
        );
    }
}

#[test]
fn required_fields_reference_declared_properties() {
    for d in definitions() {
        let name = d["name"].as_str().unwrap();
        let schema = &d["inputSchema"];
        let Some(required) = schema["required"].as_array() else {
            continue; // no required block
        };
        let props = schema["properties"].as_object().unwrap();
        for req in required {
            let key = req.as_str().expect("required entry is a string");
            assert!(
                props.contains_key(key),
                "{name}: required '{key}' is not a declared property"
            );
        }
    }
}

#[test]
fn tool_helper_builds_required_array_only_when_nonempty() {
    let with_required = tool("ghost_x", "desc", &[("a", "string", "the a", true)], &["a"]);
    assert_eq!(with_required["inputSchema"]["required"], json!(["a"]));

    let without = tool("ghost_y", "desc", &[("a", "string", "the a", false)], &[]);
    assert!(
        without["inputSchema"].get("required").is_none(),
        "empty required must be omitted, not an empty array"
    );
}

#[test]
fn known_perception_and_action_tools_are_present() {
    let defs = definitions();
    let names: HashSet<&str> = defs.iter().map(|d| d["name"].as_str().unwrap()).collect();
    for expected in [
        "ghost_context",
        "ghost_click",
        "ghost_type",
        "ghost_snapshot",
        "ghost_recipes",
        "ghost_learn_start",
    ] {
        assert!(names.contains(expected), "missing tool: {expected}");
    }
}

fn schema_of(name: &str) -> Value {
    definitions()
        .into_iter()
        .find(|d| d["name"] == name)
        .unwrap_or_else(|| panic!("{name} is defined"))["inputSchema"]
        .clone()
}

#[test]
fn semantic_action_params_are_optional_additions() {
    let expected: &[(&str, &[&str])] = &[
        ("ghost_snapshot", &["window", "snapshot", "root"]),
        ("ghost_click", &["action", "value", "allow_hid", "snapshot"]),
        (
            "ghost_type",
            &["replace", "verify", "allow_hid", "snapshot"],
        ),
        ("ghost_press", &["force"]),
        ("ghost_hotkey", &["force"]),
    ];
    for (tool, params) in expected {
        let schema = schema_of(tool);
        let required: Vec<&str> = schema["required"]
            .as_array()
            .map(|r| r.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        for param in *params {
            assert!(
                schema["properties"].get(*param).is_some(),
                "{tool}: missing '{param}'"
            );
            assert!(
                !required.contains(param),
                "{tool}: '{param}' must be optional"
            );
        }
    }
}

#[test]
fn click_action_enum_lists_the_ten_semantic_actions() {
    let schema = schema_of("ghost_click");
    let actions: Vec<&str> = schema["properties"]["action"]["enum"]
        .as_array()
        .expect("action is an enum")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(
        actions,
        [
            "press",
            "focus",
            "toggle",
            "check",
            "uncheck",
            "expand",
            "collapse",
            "select",
            "scroll_into_view",
            "show_menu"
        ]
    );
}

#[test]
fn action_descriptions_make_no_unbacked_claims() {
    for d in definitions() {
        let text = d["description"].as_str().unwrap_or_default().to_lowercase();
        assert!(!text.contains("post-click context"), "{}", d["name"]);
        assert!(!text.contains("readback verification"), "{}", d["name"]);
    }
}
