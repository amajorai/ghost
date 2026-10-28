// MCP JSON-RPC server over stdio.
// Transport: auto-detect Content-Length (Claude Code) vs NDJSON (Claude Desktop).
// stdout is used exclusively for MCP protocol. All logging goes to stderr via tracing.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};

use super::{dispatch, tools};

const PROTOCOL_VERSION: &str = "2024-11-05";
const MODERN_PROTOCOL_VERSION: &str = "2026-07-28";
const PROTOCOL_VERSION_META_KEY: &str = "io.modelcontextprotocol/protocolVersion";
const CLIENT_INFO_META_KEY: &str = "io.modelcontextprotocol/clientInfo";
const CLIENT_CAPABILITIES_META_KEY: &str = "io.modelcontextprotocol/clientCapabilities";
const SERVER_INFO_META_KEY: &str = "io.modelcontextprotocol/serverInfo";
const SERVER_NAME: &str = "ghost";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

fn cursor_identity_from_params(
    params: &serde_json::Map<String, Value>,
) -> Option<crate::tools::overlay_events::GhostCursorIdentity> {
    params
        .get("_meta")
        .and_then(|meta| meta.get("ryu/ghostCursor"))
        .and_then(|value| {
            serde_json::from_value::<crate::tools::overlay_events::GhostCursorIdentity>(
                value.clone(),
            )
            .ok()
        })
        .and_then(crate::tools::overlay_events::GhostCursorIdentity::sanitized)
}

const INSTRUCTIONS: &str = "\
Ghost gives you eyes and hands on any desktop app. \
Call ghost_recipes first for multi-step tasks. \
Call ghost_context before acting on any app. \
Use ghost_find to locate elements. \
Always pass the app parameter to action tools. \
Use ghost_annotate for visual orientation (numbered labels on screenshot). \
Use ghost_ground when AX tree returns generic elements.";

/// One read from the stdio transport. Only `Eof` (or an unrecoverable framing/I/O
/// failure) ends the server; a frame whose body is not JSON is a `ParseError`, answered
/// with JSON-RPC -32700 while the connection stays open.
#[derive(Debug)]
enum Frame {
    Message(Value),
    ParseError,
    Eof,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Transport {
    Unknown,
    ContentLength,
    NdJson,
}

pub async fn run() {
    tracing::info!("Ghost MCP server v{SERVER_VERSION} starting");

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();

    let mut reader = BufReader::new(stdin.lock());
    let mut out = stdout.lock();
    let mut transport = Transport::Unknown;

    loop {
        match read_message(&mut reader, &mut transport) {
            Frame::Eof => break,
            Frame::ParseError => {
                tracing::warn!("discarding an MCP frame that is not valid JSON");
                write_message(&mut out, &parse_error_response(), transport);
            }
            Frame::Message(msg) => {
                let response = handle_message(msg).await;
                if let Some(resp) = response {
                    write_message(&mut out, &resp, transport);
                }
            }
        }
    }

    tracing::info!("stdin closed, shutting down");
}

fn parse_error_response() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": null,
        "error": { "code": -32700, "message": "Parse error" }
    })
}

async fn handle_message(msg: Value) -> Option<Value> {
    let method = msg["method"].as_str()?;
    let id = msg.get("id").cloned();
    let params = msg["params"].as_object().cloned().unwrap_or_default();
    let metadata = params.get("_meta");
    if metadata.is_some_and(|value| !value.is_object()) {
        return id.map(|id| {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32602, "message": "MCP request _meta must be an object" }
            })
        });
    }
    let protocol_version_value = metadata
        .and_then(Value::as_object)
        .and_then(|meta| meta.get(PROTOCOL_VERSION_META_KEY));
    let protocol_version = protocol_version_value.and_then(Value::as_str);
    if protocol_version_value.is_some() && protocol_version.is_none() {
        return id.map(|id| {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32602, "message": "MCP protocolVersion must be a string" }
            })
        });
    }
    let modern = protocol_version == Some(MODERN_PROTOCOL_VERSION);

    if modern {
        let metadata = metadata.and_then(Value::as_object);
        let has_capabilities = metadata
            .and_then(|meta| meta.get(CLIENT_CAPABILITIES_META_KEY))
            .is_some_and(Value::is_object);
        let client_info_is_valid = metadata
            .and_then(|meta| meta.get(CLIENT_INFO_META_KEY))
            .is_none_or(|client_info| {
                let valid_field = |field: &str| {
                    client_info
                        .get(field)
                        .and_then(Value::as_str)
                        .is_some_and(|value| {
                            !value.trim().is_empty()
                                && value.len() <= 256
                                && !value.chars().any(char::is_control)
                        })
                };
                client_info.is_object() && valid_field("name") && valid_field("version")
            });
        if !has_capabilities || !client_info_is_valid {
            return id.map(|id| {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32602, "message": "Invalid modern MCP request metadata" }
                })
            });
        }
    }

    if let Some(requested) = protocol_version.filter(|version| *version != MODERN_PROTOCOL_VERSION)
    {
        return id.map(|id| {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32022,
                    "message": "Unsupported protocol version",
                    "data": {
                        "supported": [MODERN_PROTOCOL_VERSION, PROTOCOL_VERSION],
                        "requested": requested,
                    }
                }
            })
        });
    }

    match method {
        "initialize" if !modern => id.map(|id| {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
                    "instructions": INSTRUCTIONS,
                }
            })
        }),

        "initialize" => id.map(|id| {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": "Method not found: initialize" }
            })
        }),

        "server/discover" if modern => id.map(|id| {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": modern_result(json!({
                    "supportedVersions": [MODERN_PROTOCOL_VERSION, PROTOCOL_VERSION],
                    "capabilities": { "tools": {} },
                    "instructions": INSTRUCTIONS,
                }), true)
            })
        }),

        "server/discover" => id.map(|id| {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": "Method not found: server/discover" }
            })
        }),

        "notifications/initialized" => {
            tracing::info!("MCP client initialized");
            None
        }

        "tools/list" => id.map(|id| {
            let mut definitions = tools::definitions();
            definitions.sort_by(|left, right| {
                left["name"]
                    .as_str()
                    .unwrap_or_default()
                    .cmp(right["name"].as_str().unwrap_or_default())
            });
            let result = if modern {
                modern_result(json!({ "tools": definitions }), true)
            } else {
                json!({ "tools": definitions })
            };
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result
            })
        }),

        "tools/call" => {
            let Some(id) = id else { return None };
            let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let tool_input = params.get("arguments").cloned().unwrap_or(json!({}));
            let cursor_identity = cursor_identity_from_params(&params);

            let result =
                dispatch::dispatch_with_cursor_identity(tool_name, tool_input, cursor_identity)
                    .await;
            let result = match result {
                Ok(content) => json!({
                    "content": [{ "type": "text", "text": content.to_string() }]
                }),
                Err(e) => json!({
                    "content": [{ "type": "text", "text": format!("Error: {}", e) }],
                    "isError": true
                }),
            };
            let result = if modern {
                modern_result(result, false)
            } else {
                result
            };
            Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result
            }))
        }

        "ping" if !modern => id.map(|id| json!({ "jsonrpc": "2.0", "id": id, "result": {} })),

        "ping" => id.map(|id| {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": "Method not found: ping" }
            })
        }),

        _ => id.map(|id| {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("Method not found: {method}") }
            })
        }),
    }
}

fn modern_result(mut result: Value, cacheable: bool) -> Value {
    let Some(object) = result.as_object_mut() else {
        return result;
    };
    object.insert("resultType".into(), json!("complete"));
    let server_info = json!({
        "name": SERVER_NAME,
        "version": SERVER_VERSION,
    });
    let metadata = object.entry("_meta").or_insert_with(|| json!({}));
    if !metadata.is_object() {
        *metadata = json!({});
    }
    if let Some(metadata) = metadata.as_object_mut() {
        metadata.insert(SERVER_INFO_META_KEY.into(), server_info);
    }
    if cacheable {
        object.insert("ttlMs".into(), json!(0));
        object.insert("cacheScope".into(), json!("private"));
    }
    result
}

// ─── I/O ─────────────────────────────────────────────────────────────────────

fn read_message(reader: &mut BufReader<impl std::io::Read>, transport: &mut Transport) -> Frame {
    if *transport == Transport::Unknown {
        // Peek first byte to detect transport
        let Some(first) = peek_first_byte(reader) else {
            return Frame::Eof;
        };
        *transport = if first == b'C' {
            Transport::ContentLength
        } else {
            Transport::NdJson
        };
    }

    match transport {
        Transport::ContentLength => read_content_length(reader),
        Transport::NdJson | Transport::Unknown => read_ndjson(reader),
    }
}

fn peek_first_byte(reader: &mut BufReader<impl std::io::Read>) -> Option<u8> {
    let buf = reader.fill_buf().ok()?;
    buf.first().copied()
}

fn read_content_length(reader: &mut BufReader<impl std::io::Read>) -> Frame {
    // Read header lines until blank line
    let mut content_length: Option<usize> = None;
    let mut line = Vec::new();
    loop {
        line.clear();
        // `read_until` already retries `Interrupted`; any other error is fatal.
        let n = match reader.read_until(b'\n', &mut line) {
            Ok(n) => n,
            Err(e) => {
                tracing::error!("stdin read error: {e}");
                return Frame::Eof;
            }
        };
        if n == 0 {
            return Frame::Eof;
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end_matches(['\r', '\n']);
        if text.is_empty() {
            break;
        }
        if let Some(rest) = text.strip_prefix("Content-Length: ") {
            content_length = rest.trim().parse().ok();
        }
    }

    // Without a usable length the next frame boundary is unknown, so resyncing would
    // misread the body as headers: treat it as fatal.
    let Some(length) = content_length else {
        tracing::error!("MCP frame has a missing or invalid Content-Length header");
        return Frame::Eof;
    };
    let mut body = vec![0u8; length];
    use std::io::Read;
    if let Err(e) = reader.read_exact(&mut body) {
        if e.kind() != std::io::ErrorKind::UnexpectedEof {
            tracing::error!("stdin read error: {e}");
        }
        return Frame::Eof;
    }
    // The whole body was consumed, so a bad body is recoverable.
    serde_json::from_slice(&body).map_or(Frame::ParseError, Frame::Message)
}

fn read_ndjson(reader: &mut BufReader<impl std::io::Read>) -> Frame {
    let mut line = Vec::new();
    loop {
        line.clear();
        // Bytes, not `read_line`: invalid UTF-8 becomes an ordinary parse error, and the
        // reader stays aligned on the next newline. `read_until` retries `Interrupted`.
        let n = match reader.read_until(b'\n', &mut line) {
            Ok(n) => n,
            Err(e) => {
                tracing::error!("stdin read error: {e}");
                return Frame::Eof;
            }
        };
        if n == 0 {
            return Frame::Eof;
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        return serde_json::from_slice(&line).map_or(Frame::ParseError, Frame::Message);
    }
}

fn write_message(out: &mut impl Write, value: &Value, transport: Transport) {
    let data = match serde_json::to_vec(value) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("Serialize error: {e}");
            return;
        }
    };

    let result = match transport {
        Transport::ContentLength => write!(out, "Content-Length: {}\r\n\r\n", data.len())
            .and_then(|_| out.write_all(&data))
            .and_then(|_| out.flush()),
        _ => out
            .write_all(&data)
            .and_then(|_| out.write_all(b"\n"))
            .and_then(|_| out.flush()),
    };

    if let Err(e) = result {
        tracing::error!("Write error: {e}");
    }
}

#[cfg(test)]
mod tests {
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
}
