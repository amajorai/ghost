use super::*;

#[test]
fn event_payload_shape_is_stable() {
    // Golden: the exact JSON the overlay contract depends on. Fixed seq/ts so the
    // snapshot is deterministic.
    let event = build_event(
        7,
        "down",
        512,
        384,
        "ghost_click",
        &GhostIntent::click(Some(" Save ")),
        1_700_000_000_000,
        None,
    );
    let value = serde_json::to_value(&event).expect("serializes");
    assert_eq!(
        value,
        serde_json::json!({
            "seq": 7,
            "phase": "down",
            "x": 512,
            "y": 384,
            "tool": "ghost_click",
            "intent": "Click “Save”",
            "kind": "click",
            "target": "Save",
            "ts": 1_700_000_000_000u128,
        })
    );
}

#[test]
fn every_phase_serializes_to_its_literal() {
    for phase in ["move", "down", "up", "type", "scroll", "done"] {
        let event = build_event(
            0,
            phase,
            0,
            0,
            "ghost_type",
            &GhostIntent::for_tool("ghost_type"),
            0,
            None,
        );
        let value = serde_json::to_value(&event).expect("serializes");
        assert_eq!(value["phase"], serde_json::json!(phase));
    }
}

#[test]
fn intent_is_bounded_without_splitting_unicode() {
    let target = "界".repeat(100);
    let event = build_event(
        0,
        "down",
        0,
        0,
        "ghost_click",
        &GhostIntent::click(Some(&target)),
        0,
        None,
    );
    assert_eq!(event.intent.chars().count(), MAX_INTENT_CHARS);
    assert!(event.intent.ends_with('…'));
    let target = event.target.expect("a click keeps its target");
    assert_eq!(target.chars().count(), MAX_INTENT_CHARS);
    assert!(target.ends_with('…'));
}

#[test]
fn every_tool_maps_to_a_stable_intent_kind() {
    let cases = [
        ("ghost_click", "clicking", "Clicking"),
        ("ghost_type", "typing", "Typing"),
        ("ghost_scroll", "scrolling", "Scrolling"),
        ("ghost_drag", "dragging", "Dragging"),
        ("ghost_hover", "hovering", "Hovering"),
        ("ghost_long_press", "holding", "Holding"),
        ("ghost_wait", "working", "Working"),
    ];
    for (tool, kind, prose) in cases {
        let intent = GhostIntent::for_tool(tool);
        assert_eq!(intent.kind(), kind, "{tool}");
        assert_eq!(intent.prose(), prose, "{tool}");
        let event = build_event(0, "move", 0, 0, tool, &intent, 0, None);
        let value = serde_json::to_value(event).expect("serializes");
        assert_eq!(value["kind"], kind);
        assert!(value.get("target").is_none(), "{tool} has no target");
    }
    for (kind, _) in INTENT_KINDS {
        assert!(
            kind.chars().all(|c| c.is_ascii_lowercase()),
            "kinds are stable ascii ids: {kind}"
        );
    }
}

#[test]
fn a_click_without_a_target_is_a_bare_click() {
    for target in [None, Some(""), Some("   ")] {
        let intent = GhostIntent::click(target);
        assert_eq!(intent.kind(), "click");
        assert_eq!(intent.prose(), "Click");
        let event = build_event(0, "down", 0, 0, "ghost_click", &intent, 0, None);
        assert_eq!(event.target, None);
    }
}

#[test]
fn now_millis_reads_the_wall_clock() {
    assert!(now_millis() > 1_600_000_000_000);
}

#[test]
fn event_carries_the_trusted_cursor_identity_when_available() {
    let identity = GhostCursorIdentity {
        agent_id: Some("@ryu/codex".into()),
        agent_name: Some("Codex".into()),
        agent_title: Some("Software engineer".into()),
        session_title: Some("Polish the computer-use cursor".into()),
        avatar_url: Some("data:image/png;base64,AA==".into()),
        avatar_emoji: None,
    };
    let event = build_event(
        1,
        "move",
        12,
        24,
        "ghost_click",
        &GhostIntent::click(Some("Save")),
        0,
        Some(identity),
    );
    let value = serde_json::to_value(event).expect("serializes");
    assert_eq!(value["identity"]["agentId"], "@ryu/codex");
    assert_eq!(value["identity"]["agentName"], "Codex");
    assert_eq!(value["identity"]["agentTitle"], "Software engineer");
    assert_eq!(
        value["identity"]["sessionTitle"],
        "Polish the computer-use cursor"
    );
    assert_eq!(value["identity"]["avatarUrl"], "data:image/png;base64,AA==");
}

#[tokio::test]
async fn cursor_identity_is_scoped_to_one_action_future() {
    let identity = GhostCursorIdentity {
        agent_id: Some("@ryu/codex".into()),
        agent_name: Some("Codex".into()),
        ..Default::default()
    };
    let scoped = with_cursor_identity(Some(identity.clone()), async {
        CURSOR_IDENTITY
            .try_with(Clone::clone)
            .expect("identity is available inside the action")
    })
    .await;
    assert_eq!(scoped, Some(identity));
    assert!(CURSOR_IDENTITY.try_with(Clone::clone).is_err());
}

#[test]
fn cursor_identity_bounds_text_and_drops_unsafe_avatar_urls() {
    let identity = GhostCursorIdentity {
        agent_name: Some("x".repeat(100)),
        session_title: Some("t".repeat(200)),
        avatar_url: Some("javascript:alert(1)".into()),
        ..Default::default()
    }
    .sanitized()
    .expect("text identity remains useful");

    assert_eq!(identity.agent_name.as_deref().map(str::len), Some(80));
    assert_eq!(identity.session_title.as_deref().map(str::len), Some(160));
    assert_eq!(identity.avatar_url, None);
}

// Run under a runtime so the emit path is safe whether or not an overlay URL is set
// in the environment (a set URL would `tokio::spawn` the detached POST).
#[tokio::test]
async fn emit_and_press_helpers_never_panic() {
    // With no overlay URL these are pure no-ops; the point is that narrating an
    // action must never fail or block the caller regardless of overlay presence.
    emit("move", 1, 2, "ghost_click");
    press_start(3, 4, "ghost_click");
    press_end(5, 6, "ghost_click");
}
