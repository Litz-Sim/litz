//! A Model Context Protocol server for Litz, over stdio.
//!
//! The transport is newline-delimited JSON-RPC 2.0: one message per line, no
//! HTTP framing and no JavaScript runtime. The tools call the same crates the
//! `litz` command uses — `litz-catalog` and `litz-diagram` directly, and the
//! runner through [`Runtime`], which the CLI installs before serving. Nothing
//! here reimplements a simulation.
//!
//! ```
//! use std::io::Cursor;
//!
//! let input = concat!(
//!     r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
//!     "\n",
//!     r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
//!     "\n",
//! );
//! let mut output = Vec::new();
//! litz_mcp::serve_stdio(Cursor::new(input.as_bytes()), &mut output).unwrap();
//! assert!(String::from_utf8(output).unwrap().contains("\"litz_check\""));
//! ```
#![forbid(unsafe_code)]

mod paths;
mod runtime;
mod tools;

pub use runtime::{RunReport, Runtime, TestEntry, TestReport, install_runtime};

use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{self, BufRead, Write};

/// The newest protocol revision this server speaks. An `initialize` request
/// naming an older revision is answered with that revision.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
pub const SERVER_NAME: &str = "litz-mcp";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Deserialize)]
struct Request {
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    params: Value,
}

/// Serve MCP messages read from `reader` and written to `writer` until EOF.
///
/// A caller runs this on the process's standard streams; tests pass in-memory
/// buffers. Notifications produce no output, and a clean EOF returns `Ok(())`.
pub fn serve_stdio<R: BufRead, W: Write>(mut reader: R, mut writer: W) -> io::Result<()> {
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            return Ok(());
        }
        let text = line.trim();
        if text.is_empty() {
            continue;
        }
        if let Some(response) = handle(text) {
            serde_json::to_writer(&mut writer, &response).map_err(io::Error::other)?;
            writer.write_all(b"\n")?;
            writer.flush()?;
        }
    }
}

fn handle(text: &str) -> Option<Value> {
    let request: Request = match serde_json::from_str(text) {
        Ok(request) => request,
        Err(error) => {
            return Some(error_response(
                Value::Null,
                -32700,
                &format!("parse error: {error}"),
            ));
        }
    };

    let Some(method) = request.method.as_deref() else {
        return request
            .id
            .map(|id| error_response(id, -32600, "invalid request: missing method"));
    };
    let id = request.id;
    let is_notification = id.is_none();

    let outcome = match method {
        "initialize" => Ok(initialize(&request.params)),
        "notifications/initialized" | "notifications/cancelled" | "initialized" => Ok(Value::Null),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tools::list()),
        "tools/call" => tools_call(&request.params),
        other => Err((-32601, format!("method not found: {other}"))),
    };

    if is_notification {
        return None;
    }
    let id = id.unwrap_or(Value::Null);
    match outcome {
        Ok(Value::Null) => None,
        Ok(result) => Some(json!({"jsonrpc": "2.0", "id": id, "result": result})),
        Err((code, message)) => Some(error_response(id, code, &message)),
    }
}

fn initialize(params: &Value) -> Value {
    let version = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(PROTOCOL_VERSION);
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {"listChanged": false}},
        "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION},
        "instructions": "Litz simulates ESP32 firmware. Every tool takes a project `root`; all other paths are relative to it."
    })
}

fn tools_call(params: &Value) -> Result<Value, (i64, String)> {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return Err((-32602, "tools/call needs a string \"name\"".into()));
    };
    if !tools::is_known(name) {
        return Err((-32602, format!("unknown tool {name:?}")));
    }
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !arguments.is_object() {
        return Err((-32602, "\"arguments\" must be an object".into()));
    }

    match tools::call(name, &arguments) {
        Ok(value) => {
            let text = match serde_json::to_string_pretty(&value) {
                Ok(text) => text,
                Err(_) => value.to_string(),
            };
            Ok(json!({
                "content": [{"type": "text", "text": text}],
                "structuredContent": value,
                "isError": false
            }))
        }
        Err(message) => Ok(json!({
            "content": [{"type": "text", "text": message}],
            "isError": true
        })),
    }
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message}
    })
}
