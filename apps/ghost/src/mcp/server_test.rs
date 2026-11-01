//! MCP server tests: transport framing, protocol negotiation and tool listing.

use super::*;
use std::io::Cursor;

#[test]
fn tools_call_reads_the_cursor_identity_from_mcp_meta() {
    let message = json!({
        "name": "ghost_click",
        "arguments": { "query": "Send" },
        "_meta": {
            "ryu/ghostCursor": {
                "agentId": "@ryu/codex",
                "agentName": "Codex",
                "sessionTitle": "Polish the computer-use cursor",
                "avatarEmoji": "✦"
            }
        }
    });
    let params = message.as_object().expect("params object");
    let identity = cursor_identity_from_params(params).expect("cursor identity");
    assert_eq!(identity.agent_id.as_deref(), Some("@ryu/codex"));
    assert_eq!(identity.agent_name.as_deref(), Some("Codex"));
    assert_eq!(
        identity.session_title.as_deref(),
        Some("Polish the computer-use cursor")
    );
    assert_eq!(identity.avatar_emoji.as_deref(), Some("✦"));
}

#[test]
fn tool_arguments_cannot_supply_the_trusted_cursor_identity() {
    let message = json!({
        "name": "ghost_click",
        "arguments": { "agentName": "Spoofed" }
    });
    let params = message.as_object().expect("params object");
    assert!(cursor_identity_from_params(params).is_none());
}

#[tokio::test]
async fn initialize_returns_protocol_and_server_info() {
    let msg = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} });
    let resp = handle_message(msg).await.expect("initialize replies");
    assert_eq!(resp["id"], 1);
    assert_eq!(resp["result"]["protocolVersion"], PROTOCOL_VERSION);
    assert_eq!(resp["result"]["serverInfo"]["name"], SERVER_NAME);
    assert_eq!(resp["result"]["serverInfo"]["version"], SERVER_VERSION);
    assert!(resp["result"]["instructions"].as_str().is_some());
}

fn modern_request(id: u64, method: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": {
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": MODERN_PROTOCOL_VERSION,
                "io.modelcontextprotocol/clientInfo": { "name": "test-client", "version": "1" },
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        }
    })
}

#[tokio::test]
async fn modern_discovery_returns_identity_capabilities_and_cache_hints() {
    let response = handle_message(modern_request(1, "server/discover"))
        .await
        .expect("server/discover replies without initialize");

    assert_eq!(response["result"]["resultType"], "complete");
    assert_eq!(
        response["result"]["supportedVersions"],
        json!([MODERN_PROTOCOL_VERSION, PROTOCOL_VERSION])
    );
    assert_eq!(response["result"]["capabilities"]["tools"], json!({}));
    assert_eq!(
        response["result"]["_meta"][SERVER_INFO_META_KEY]["name"],
        SERVER_NAME
    );
    assert_eq!(
        response["result"]["_meta"][SERVER_INFO_META_KEY]["version"],
        SERVER_VERSION
    );
    assert_eq!(response["result"]["ttlMs"], 0);
    assert_eq!(response["result"]["cacheScope"], "private");
}

#[tokio::test]
async fn modern_requests_are_stateless_and_tool_lists_are_deterministic() {
    let first = handle_message(modern_request(2, "tools/list"))
        .await
        .expect("modern tools/list replies without initialize");
    let second = handle_message(modern_request(3, "tools/list"))
        .await
        .expect("modern tools/list can be repeated independently");
    let tools = first["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect();

    assert!(names.windows(2).all(|pair| pair[0] <= pair[1]));
    assert_eq!(first["result"]["tools"], second["result"]["tools"]);
    assert_eq!(first["result"]["resultType"], "complete");
    assert_eq!(
        first["result"]["_meta"][SERVER_INFO_META_KEY]["name"],
        SERVER_NAME
    );
    assert_eq!(first["result"]["ttlMs"], 0);
    assert_eq!(first["result"]["cacheScope"], "private");
}

#[tokio::test]
async fn modern_tool_call_returns_a_complete_result_without_a_session() {
    let mut request = modern_request(5, "tools/call");
    request["params"]["name"] = json!("ghost_not_a_tool");
    request["params"]["arguments"] = json!({});
    let response = handle_message(request)
        .await
        .expect("modern tools/call replies without initialize");

    assert_eq!(response["result"]["resultType"], "complete");
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(
        response["result"]["_meta"][SERVER_INFO_META_KEY]["name"],
        SERVER_NAME
    );
}

#[tokio::test]
async fn malformed_modern_metadata_is_rejected_at_the_request_boundary() {
    let mut request = modern_request(6, "server/discover");
    request["params"]["_meta"][CLIENT_CAPABILITIES_META_KEY] = json!("not-an-object");
    let response = handle_message(request)
        .await
        .expect("malformed metadata returns an error");

    assert_eq!(response["error"]["code"], -32602);
}

#[tokio::test]
async fn unsupported_modern_request_version_is_reported_with_supported_versions() {
    let mut msg = modern_request(4, "server/discover");
    msg["params"]["_meta"][PROTOCOL_VERSION_META_KEY] = json!("2027-01-01");
    let response = handle_message(msg).await.expect("version error replies");

    assert_eq!(response["error"]["code"], -32022);
    assert_eq!(response["error"]["data"]["requested"], "2027-01-01");
    assert_eq!(
        response["error"]["data"]["supported"],
        json!([MODERN_PROTOCOL_VERSION, PROTOCOL_VERSION])
    );
}

#[tokio::test]
async fn initialized_notification_has_no_reply() {
    let msg = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    assert!(handle_message(msg).await.is_none());
}

#[tokio::test]
async fn tools_list_returns_all_definitions() {
    let msg = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
    let resp = handle_message(msg).await.expect("tools/list replies");
    let tools = resp["result"]["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), tools::definitions().len());
}

#[tokio::test]
async fn ping_returns_empty_result() {
    let msg = json!({ "jsonrpc": "2.0", "id": 9, "method": "ping" });
    let resp = handle_message(msg).await.expect("ping replies");
    assert_eq!(resp["id"], 9);
    assert_eq!(resp["result"], json!({}));
}

#[tokio::test]
async fn modern_ping_is_removed_while_legacy_ping_remains_supported() {
    let response = handle_message(modern_request(10, "ping"))
        .await
        .expect("modern ping returns method-not-found");

    assert_eq!(response["error"]["code"], -32601);
    assert_eq!(response["error"]["message"], "Method not found: ping");
}

#[tokio::test]
async fn unknown_method_returns_method_not_found() {
    let msg = json!({ "jsonrpc": "2.0", "id": 3, "method": "totally/bogus" });
    let resp = handle_message(msg).await.expect("error reply");
    assert_eq!(resp["error"]["code"], -32601);
    assert!(resp["error"]["message"]
        .as_str()
        .unwrap()
        .contains("totally/bogus"));
}

#[tokio::test]
async fn message_without_method_is_ignored() {
    let msg = json!({ "jsonrpc": "2.0", "id": 5 });
    assert!(handle_message(msg).await.is_none());
}

#[tokio::test]
async fn tools_call_unknown_tool_wraps_error_result() {
    let msg = json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": { "name": "ghost_not_a_tool", "arguments": {} }
    });
    let resp = handle_message(msg).await.expect("tools/call replies");
    assert_eq!(resp["result"]["isError"], true);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("Unknown tool"), "got: {text}");
}

/// The contract codes, for the "exactly one code prefix" check.
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

fn assert_single_code_prefix(text: &str, expected: &str) {
    let rest = text
        .strip_prefix("Error: ")
        .unwrap_or_else(|| panic!("error text must start with 'Error: ', got: {text}"));
    let after = rest
        .strip_prefix(expected)
        .and_then(|r| r.strip_prefix(": "))
        .unwrap_or_else(|| panic!("expected code {expected} after 'Error: ', got: {text}"));
    for code in CODES {
        assert!(
            !after.starts_with(&format!("{code}:")),
            "duplicated code prefix in: {text}"
        );
    }
}

#[tokio::test]
async fn tool_error_carries_structured_content_and_one_code_prefix() {
    let msg = json!({
        "jsonrpc": "2.0",
        "id": 11,
        "method": "tools/call",
        "params": { "name": "ghost_not_a_tool", "arguments": {} }
    });
    let resp = handle_message(msg).await.expect("tools/call replies");
    let result = &resp["result"];
    assert_eq!(result["isError"], true);
    assert_eq!(result["structuredContent"]["ok"], false);
    let error = &result["structuredContent"]["error"];
    assert_eq!(error["code"], "UNKNOWN_TOOL");
    assert_eq!(error["disposition"]["delivery"], "not_delivered");
    assert_eq!(error["disposition"]["retry"], "safe");
    let text = result["content"][0]["text"].as_str().unwrap();
    assert_single_code_prefix(text, "UNKNOWN_TOOL");
}

#[tokio::test]
async fn invalid_args_error_is_classified_with_one_prefix() {
    let msg = json!({
        "jsonrpc": "2.0",
        "id": 12,
        "method": "tools/call",
        "params": { "name": "ghost_wait", "arguments": { "condition": "bogusCondition" } }
    });
    let resp = handle_message(msg).await.expect("tools/call replies");
    let result = &resp["result"];
    assert_eq!(result["structuredContent"]["error"]["code"], "INVALID_ARGS");
    let text = result["content"][0]["text"].as_str().unwrap();
    assert_single_code_prefix(text, "INVALID_ARGS");
}

#[test]
fn tool_error_result_maps_a_legacy_stale_ref_prefix_to_the_code() {
    let err = anyhow::anyhow!("STALE_REF: ref @e3 no longer matches");
    let result = tool_error_result(&err, "ghost_click");
    assert_eq!(result["structuredContent"]["error"]["code"], "STALE_REF");
    assert_eq!(
        result["structuredContent"]["error"]["message"],
        "ref @e3 no longer matches"
    );
    assert_single_code_prefix(result["content"][0]["text"].as_str().unwrap(), "STALE_REF");
}

#[test]
fn tool_success_result_has_no_structured_content() {
    let result = tool_success_result(&json!({ "success": true }));
    assert!(result.get("structuredContent").is_none());
    assert!(result.get("isError").is_none());
    assert_eq!(result["content"][0]["text"], r#"{"success":true}"#);
}

#[tokio::test]
async fn tools_call_without_id_is_dropped() {
    let msg = json!({
        "jsonrpc": "2.0",
        "method": "tools/call",
        "params": { "name": "ghost_not_a_tool", "arguments": {} }
    });
    assert!(handle_message(msg).await.is_none());
}

#[test]
fn read_ndjson_parses_a_line_skipping_blanks() {
    let input = b"\n\n{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}\n";
    let mut reader = BufReader::new(Cursor::new(&input[..]));
    let Frame::Message(v) = read_ndjson(&mut reader) else {
        panic!("parsed");
    };
    assert_eq!(v["id"], 7);
    assert_eq!(v["method"], "ping");
}

#[test]
fn read_ndjson_eof_returns_none() {
    let mut reader = BufReader::new(Cursor::new(&b""[..]));
    assert!(matches!(read_ndjson(&mut reader), Frame::Eof));
}

#[test]
fn read_ndjson_bad_line_is_a_parse_error_and_reading_continues() {
    let input = b"{\"truncated\":\n\xff\xfe not utf8\n{\"id\":3,\"method\":\"ping\"}\n";
    let mut reader = BufReader::new(Cursor::new(&input[..]));
    assert!(matches!(read_ndjson(&mut reader), Frame::ParseError));
    assert!(matches!(read_ndjson(&mut reader), Frame::ParseError));
    let Frame::Message(v) = read_ndjson(&mut reader) else {
        panic!("the good line after bad ones still parses");
    };
    assert_eq!(v["id"], 3);
    assert!(matches!(read_ndjson(&mut reader), Frame::Eof));
}

#[test]
fn read_content_length_bad_body_is_a_parse_error_and_reading_continues() {
    let good = r#"{"id":4,"method":"ping"}"#;
    let framed = format!(
        "Content-Length: 5\r\n\r\n{{bad}}Content-Length: {}\r\n\r\n{}",
        good.len(),
        good
    );
    let mut reader = BufReader::new(Cursor::new(framed.into_bytes()));
    assert!(matches!(
        read_content_length(&mut reader),
        Frame::ParseError
    ));
    let Frame::Message(v) = read_content_length(&mut reader) else {
        panic!("next frame parses");
    };
    assert_eq!(v["id"], 4);
}

#[test]
fn read_content_length_without_length_header_is_fatal() {
    let mut reader = BufReader::new(Cursor::new(b"Content-Type: x\r\n\r\n{}".to_vec()));
    assert!(matches!(read_content_length(&mut reader), Frame::Eof));
}

#[test]
fn parse_error_response_is_a_json_rpc_parse_error() {
    let resp = parse_error_response();
    assert_eq!(resp["error"]["code"], -32700);
    assert!(resp["id"].is_null());
}

#[test]
fn read_content_length_reads_exact_body() {
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
    let framed = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
    let mut reader = BufReader::new(Cursor::new(framed.into_bytes()));
    let Frame::Message(v) = read_content_length(&mut reader) else {
        panic!("parsed");
    };
    assert_eq!(v["method"], "ping");
}

#[test]
fn read_message_detects_content_length_from_leading_c() {
    let body = r#"{"id":1,"method":"ping"}"#;
    let framed = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
    let mut reader = BufReader::new(Cursor::new(framed.into_bytes()));
    let mut transport = Transport::Unknown;
    let Frame::Message(v) = read_message(&mut reader, &mut transport) else {
        panic!("parsed");
    };
    assert_eq!(transport, Transport::ContentLength);
    assert_eq!(v["method"], "ping");
}

#[test]
fn read_message_detects_ndjson_from_leading_brace() {
    let line = "{\"id\":2,\"method\":\"ping\"}\n";
    let mut reader = BufReader::new(Cursor::new(line.as_bytes().to_vec()));
    let mut transport = Transport::Unknown;
    let Frame::Message(v) = read_message(&mut reader, &mut transport) else {
        panic!("parsed");
    };
    assert_eq!(transport, Transport::NdJson);
    assert_eq!(v["id"], 2);
}

#[test]
fn write_message_content_length_framing() {
    let mut out: Vec<u8> = Vec::new();
    write_message(&mut out, &json!({ "ok": true }), Transport::ContentLength);
    let s = String::from_utf8(out).unwrap();
    assert!(s.starts_with("Content-Length: "));
    assert!(s.contains("\r\n\r\n"));
    assert!(s.trim_end().ends_with("{\"ok\":true}"));
}

#[test]
fn write_message_ndjson_appends_newline() {
    let mut out: Vec<u8> = Vec::new();
    write_message(&mut out, &json!({ "ok": true }), Transport::NdJson);
    let s = String::from_utf8(out).unwrap();
    assert_eq!(s, "{\"ok\":true}\n");
}
