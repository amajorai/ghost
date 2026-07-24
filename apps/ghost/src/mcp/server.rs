// MCP JSON-RPC server over stdio.
// Transport: auto-detect Content-Length (Claude Code) vs NDJSON (Claude Desktop).
// stdout is used exclusively for MCP protocol. All logging goes to stderr via tracing.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};

use super::{dispatch, tools};

const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "ghost";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

const INSTRUCTIONS: &str = "\
Ghost gives you eyes and hands on any desktop app. \
Call ghost_recipes first for multi-step tasks. \
Call ghost_context before acting on any app. \
Use ghost_find to locate elements. \
Always pass the app parameter to action tools. \
Use ghost_annotate for visual orientation (numbered labels on screenshot). \
Use ghost_ground when AX tree returns generic elements.";

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
            None => break,
            Some(msg) => {
                let response = handle_message(msg).await;
                if let Some(resp) = response {
                    write_message(&mut out, &resp, transport);
                }
            }
        }
    }

    tracing::info!("stdin closed, shutting down");
}

async fn handle_message(msg: Value) -> Option<Value> {
    let method = msg["method"].as_str()?;
    let id = msg.get("id").cloned();
    let params = msg["params"].as_object().cloned().unwrap_or_default();

    match method {
        "initialize" => id.map(|id| {
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

        "notifications/initialized" => {
            tracing::info!("MCP client initialized");
            None
        }

        "tools/list" => id.map(|id| {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "tools": tools::definitions() }
            })
        }),

        "tools/call" => {
            let Some(id) = id else { return None };
            let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let tool_input = params.get("arguments").cloned().unwrap_or(json!({}));

            let result = dispatch::dispatch(tool_name, tool_input).await;
            Some(match result {
                Ok(content) => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "content": [{ "type": "text", "text": content.to_string() }] }
                }),
                Err(e) => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{ "type": "text", "text": format!("Error: {}", e) }],
                        "isError": true
                    }
                }),
            })
        }

        "ping" => id.map(|id| json!({ "jsonrpc": "2.0", "id": id, "result": {} })),

        _ => id.map(|id| {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("Method not found: {method}") }
            })
        }),
    }
}

// ─── I/O ─────────────────────────────────────────────────────────────────────

fn read_message(
    reader: &mut BufReader<impl std::io::Read>,
    transport: &mut Transport,
) -> Option<Value> {
    if *transport == Transport::Unknown {
        // Peek first byte to detect transport
        let first = peek_first_byte(reader)?;
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

fn read_content_length(reader: &mut BufReader<impl std::io::Read>) -> Option<Value> {
    // Read header lines until blank line
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        let line = line.trim_end_matches(|c| c == '\r' || c == '\n');
        if line.is_empty() {
            break;
        }
        if let Some(rest) = line.strip_prefix("Content-Length: ") {
            content_length = rest.trim().parse().ok();
        }
    }

    let length = content_length?;
    let mut body = vec![0u8; length];
    use std::io::Read;
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn read_ndjson(reader: &mut BufReader<impl std::io::Read>) -> Option<Value> {
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).ok()?;
        if n == 0 {
            return None;
        }
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            return serde_json::from_str(trimmed).ok();
        }
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
        let v = read_ndjson(&mut reader).expect("parsed");
        assert_eq!(v["id"], 7);
        assert_eq!(v["method"], "ping");
    }

    #[test]
    fn read_ndjson_eof_returns_none() {
        let mut reader = BufReader::new(Cursor::new(&b""[..]));
        assert!(read_ndjson(&mut reader).is_none());
    }

    #[test]
    fn read_content_length_reads_exact_body() {
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let framed = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        let mut reader = BufReader::new(Cursor::new(framed.into_bytes()));
        let v = read_content_length(&mut reader).expect("parsed");
        assert_eq!(v["method"], "ping");
    }

    #[test]
    fn read_message_detects_content_length_from_leading_c() {
        let body = r#"{"id":1,"method":"ping"}"#;
        let framed = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        let mut reader = BufReader::new(Cursor::new(framed.into_bytes()));
        let mut transport = Transport::Unknown;
        let v = read_message(&mut reader, &mut transport).expect("parsed");
        assert_eq!(transport, Transport::ContentLength);
        assert_eq!(v["method"], "ping");
    }

    #[test]
    fn read_message_detects_ndjson_from_leading_brace() {
        let line = "{\"id\":2,\"method\":\"ping\"}\n";
        let mut reader = BufReader::new(Cursor::new(line.as_bytes().to_vec()));
        let mut transport = Transport::Unknown;
        let v = read_message(&mut reader, &mut transport).expect("parsed");
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
