// Integration test: the MCP error envelope of `ghost mcp` for action tools.
//
// Every call here is refused before any input, focus or accessibility walk: a ref
// that cannot resolve, malformed arguments, and a blocked hotkey (the policy runs
// before the app is focused). The child runs headless with an isolated data dir and
// no sealing key, so its snapshot store is empty and in-memory.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};

const CODES: &[&str] = &[
    "STALE_REF",
    "AMBIGUOUS_TARGET",
    "ELEMENT_NOT_FOUND",
    "ACTION_NOT_SUPPORTED",
    "ACTION_FAILED",
    "POLICY_DENIED",
    "PERM_DENIED",
    "TIMEOUT",
    "INVALID_ARGS",
    "APP_NOT_FOUND",
    "APP_UNRESPONSIVE",
    "PLATFORM_NOT_SUPPORTED",
    "UNKNOWN_TOOL",
    "INTERNAL",
];

struct Ghost {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    next_id: u64,
}

impl Ghost {
    fn spawn() -> Self {
        let data_dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("ghost-error-contract-{}", std::process::id()));
        let mut child = Command::new(env!("CARGO_BIN_EXE_ghost"))
            .arg("mcp")
            .env("RYU_DIR", &data_dir)
            .env("RYU_KEYCHAIN", "off")
            .env("RYU_GHOST_HEADLESS", "1")
            .env_remove("RYU_MASTER_KEY")
            .env_remove("RYU_SEAL_KEY")
            .env_remove("RYU_GHOST_OVERLAY_URL")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn ghost mcp");
        let stdin = child.stdin.take().expect("stdin");
        let reader = BufReader::new(child.stdout.take().expect("stdout"));
        let mut ghost = Self {
            child,
            stdin,
            reader,
            next_id: 1,
        };
        let init = ghost.request("initialize", json!({}));
        assert_eq!(init["result"]["serverInfo"]["name"], "ghost");
        ghost.send(
            &json!({ "jsonrpc": "2.0", "method": "notifications/initialized", "params": {} }),
        );
        ghost
    }

    fn send(&mut self, msg: &Value) {
        self.stdin.write_all(msg.to_string().as_bytes()).unwrap();
        self.stdin.write_all(b"\n").unwrap();
        self.stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let mut line = String::new();
        self.reader.read_line(&mut line).unwrap();
        let response: Value = serde_json::from_str(line.trim()).expect("valid JSON response");
        assert_eq!(response["id"], id);
        response
    }

    /// Call a tool and return its `result`, asserting it is an error envelope.
    fn call_err(&mut self, tool: &str, arguments: Value) -> Value {
        let response = self.request(
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        );
        let result = response["result"].clone();
        assert_eq!(result["isError"], true, "{tool} should fail: {result}");
        assert_eq!(result["structuredContent"]["ok"], false, "{result}");
        result
    }
}

impl Drop for Ghost {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The error code, after checking the text carries it as exactly one prefix.
fn checked_code(result: &Value) -> String {
    let error = &result["structuredContent"]["error"];
    let code = error["code"].as_str().expect("error.code").to_string();
    let text = result["content"][0]["text"].as_str().expect("content text");
    assert!(
        text.starts_with(&format!("Error: {code}: ")),
        "text must start with one code prefix: {text}"
    );
    let rest = &text[format!("Error: {code}: ").len()..];
    for other in CODES {
        assert!(
            !rest.starts_with(&format!("{other}:")),
            "duplicated code prefix: {text}"
        );
    }
    assert_eq!(error["message"], rest, "{result}");
    code
}

fn disposition(result: &Value) -> (String, String) {
    let d = &result["structuredContent"]["error"]["disposition"];
    (
        d["delivery"].as_str().unwrap_or_default().to_string(),
        d["retry"].as_str().unwrap_or_default().to_string(),
    )
}

/// A blocked combo on this OS: (modifier, key).
fn blocked_combo() -> (&'static str, &'static str) {
    if cfg!(target_os = "macos") {
        ("cmd", "q")
    } else {
        ("alt", "f4")
    }
}

#[test]
fn action_errors_follow_the_contract() {
    let mut ghost = Ghost::spawn();

    // A ref that cannot resolve is STALE_REF, not delivered, safe to retry.
    let stale = ghost.call_err("ghost_click", json!({ "ref": "@e999" }));
    assert_eq!(checked_code(&stale), "STALE_REF");
    assert_eq!(
        disposition(&stale),
        ("not_delivered".to_string(), "safe".to_string())
    );

    // Malformed ref.
    let bad_ref = ghost.call_err("ghost_click", json!({ "ref": "@bad:x" }));
    assert_eq!(checked_code(&bad_ref), "INVALID_ARGS");
    assert_eq!(disposition(&bad_ref).0, "not_delivered");

    // Unknown wait condition.
    let bogus = ghost.call_err("ghost_wait", json!({ "condition": "bogus" }));
    assert_eq!(checked_code(&bogus), "INVALID_ARGS");

    // A blocked hotkey is refused by the policy, before any focus or input.
    let (modifier, key) = blocked_combo();
    let hotkey = ghost.call_err(
        "ghost_hotkey",
        json!({ "keys": [modifier, key], "app": "GhostErrorContractNoSuchApp" }),
    );
    assert_eq!(checked_code(&hotkey), "POLICY_DENIED");
    assert_eq!(
        disposition(&hotkey),
        ("not_delivered".to_string(), "safe".to_string())
    );
    let message = hotkey["structuredContent"]["error"]["message"]
        .as_str()
        .unwrap_or_default();
    assert!(message.contains("is blocked"), "{message}");

    let press = ghost.call_err(
        "ghost_press",
        json!({ "key": key, "modifiers": [modifier], "app": "GhostErrorContractNoSuchApp" }),
    );
    assert_eq!(checked_code(&press), "POLICY_DENIED");
}
