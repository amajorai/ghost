// MCP JSON-RPC server over stdio.
// Transport: auto-detect Content-Length (Claude Code) vs NDJSON (Claude Desktop).
// stdout is used exclusively for MCP protocol. All logging goes to stderr via tracing.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};

use super::result::{tool_error_result, tool_success_result};
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
            let id = id?;
            let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let tool_input = params.get("arguments").cloned().unwrap_or(json!({}));
            let cursor_identity = cursor_identity_from_params(&params);

            let result =
                dispatch::dispatch_with_cursor_identity(tool_name, tool_input, cursor_identity)
                    .await;
            let result = match result {
                Ok(content) => tool_success_result(&content),
                Err(e) => tool_error_result(&e, tool_name),
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
#[path = "server_test.rs"]
mod tests;
