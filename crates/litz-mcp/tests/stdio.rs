//! The protocol in memory: no subprocess, no sockets, just buffers.

use serde_json::{Value, json};
use std::io::Cursor;

fn serve(messages: &[Value]) -> Vec<Value> {
    let mut input = String::new();
    for message in messages {
        input.push_str(&serde_json::to_string(message).expect("a request serialises"));
        input.push('\n');
    }
    let mut output = Vec::new();
    litz_mcp::serve_stdio(Cursor::new(input.into_bytes()), &mut output)
        .expect("in-memory serving cannot fail");
    String::from_utf8(output)
        .expect("the server writes UTF-8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("every line is one JSON message"))
        .collect()
}

fn call(id: u64, name: &str, arguments: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {"name": name, "arguments": arguments}
    })
}

fn blinky_root() -> String {
    format!("{}/../../examples/blinky", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn initialize_handshake() {
    let responses = serve(&[
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "0"}
            }
        }),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}),
    ]);

    assert_eq!(responses.len(), 2, "the notification has no response");
    assert_eq!(responses[0]["id"], 1);
    assert_eq!(responses[0]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(responses[0]["result"]["serverInfo"]["name"], "litz-mcp");
    assert!(responses[0]["result"]["capabilities"]["tools"].is_object());
    assert_eq!(responses[1]["id"], 2);
    assert_eq!(responses[1]["result"], json!({}));
}

#[test]
fn tools_list_lists_every_tool() {
    let responses = serve(&[json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})]);
    let tools = responses[0]["result"]["tools"]
        .as_array()
        .expect("tools is an array");
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().expect("every tool is named"))
        .collect();
    for expected in [
        "litz_check",
        "litz_test",
        "litz_run",
        "litz_circuit_create",
        "litz_circuit_update",
        "litz_code_generate",
        "litz_import_diagram",
    ] {
        assert!(names.contains(&expected), "missing {expected}: {names:?}");
    }
    assert!(
        tools
            .iter()
            .all(|tool| tool["inputSchema"]["type"] == "object")
    );
}

#[test]
fn check_on_the_blinky_example_passes() {
    let responses = serve(&[call(3, "litz_check", json!({"root": blinky_root()}))]);
    let result = &responses[0]["result"];
    assert_eq!(result["isError"], false, "{result}");
    assert_eq!(result["structuredContent"]["passed"], true, "{result}");
    assert_eq!(result["structuredContent"]["diagram"], "diagram.json");
    assert_eq!(
        result["structuredContent"]["diagnostics"],
        json!([]),
        "the example is valid"
    );
}

#[test]
fn an_unknown_tool_is_an_invalid_params_error() {
    let responses = serve(&[call(4, "litz_does_not_exist", json!({}))]);
    assert_eq!(responses[0]["error"]["code"], -32602, "{responses:?}");
    assert!(
        responses[0]["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("unknown tool")),
        "{responses:?}"
    );
}

#[test]
fn a_path_that_escapes_the_root_is_refused() {
    let responses = serve(&[call(
        5,
        "litz_check",
        json!({"root": blinky_root(), "diagram": "../Cargo.toml"}),
    )]);
    let result = &responses[0]["result"];
    assert_eq!(result["isError"], true, "{result}");
    assert!(
        result["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("escapes the project root")),
        "{result}"
    );
}
