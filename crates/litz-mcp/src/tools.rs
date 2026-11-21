//! The tools the server exposes.
//!
//! Every tool takes a `root` naming the project directory and resolves every
//! other path against it. Validation uses the same crates the CLI uses
//! (`litz-catalog`, `litz-diagram`); test and run execution go through the
//! installed [`crate::Runtime`], which is the existing runner.

use crate::paths::ProjectRoot;
use crate::runtime;
use litz_catalog::Registry;
use litz_core::PartState;
use litz_diagram::{Circuit, Diagnostic, PartDef, PinRef, Severity};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;
use std::fs;

/// `tools/list` for the protocol handshake.
pub fn list() -> Value {
    json!({
        "tools": [
            {
                "name": "litz_check",
                "description": "Validate a Litz project's diagram against the catalog without running firmware.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "root": {"type": "string", "description": "Project directory."},
                        "diagram": {"type": "string", "description": "Diagram file relative to root; defaults to the litz.toml setting or diagram.json."},
                        "strict": {"type": "boolean", "description": "Treat warnings as failures.", "default": false}
                    },
                    "required": ["root"],
                    "additionalProperties": false
                }
            },
            {
                "name": "litz_test",
                "description": "Run the project's [[test]] entries, or a scenario, through the existing Litz runner.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "root": {"type": "string", "description": "Project directory."},
                        "scenario": {"type": "string", "description": "Scenario YAML relative to root; when omitted the [[test]] entries run."},
                        "strict": {"type": "boolean", "description": "Treat warnings as failures.", "default": false}
                    },
                    "required": ["root"],
                    "additionalProperties": false
                }
            },
            {
                "name": "litz_run",
                "description": "Boot the firmware for bounded simulated time and return serial output, exit status and part states.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "root": {"type": "string", "description": "Project directory."},
                        "timeout": {"type": "string", "description": "Simulated time limit such as \"2s\" or \"500ms\"; defaults to 10s."},
                        "strict": {"type": "boolean", "description": "Treat warnings as failures.", "default": false}
                    },
                    "required": ["root"],
                    "additionalProperties": false
                }
            },
            {
                "name": "litz_circuit_create",
                "description": "Write a diagram.json from parts and connections, using catalog ids.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "root": {"type": "string", "description": "Project directory."},
                        "path": {"type": "string", "description": "Diagram file relative to root; defaults to the litz.toml setting or diagram.json."},
                        "parts": {
                            "type": "array",
                            "description": "Placed parts.",
                            "items": {
                                "type": "object",
                                "required": ["id", "type"],
                                "properties": {
                                    "id": {"type": "string"},
                                    "type": {"type": "string", "description": "Catalog id such as litz-led."},
                                    "attrs": {"type": "object"}
                                }
                            }
                        },
                        "connections": {
                            "type": "array",
                            "description": "Wires as {from,to,color?,route?} or [from,to,color,route].",
                            "items": {"type": ["object", "array"]}
                        },
                        "force": {"type": "boolean", "description": "Replace an existing diagram.", "default": false}
                    },
                    "required": ["root", "parts"],
                    "additionalProperties": false
                }
            },
            {
                "name": "litz_circuit_update",
                "description": "Merge parts and connections into an existing diagram, preserving fields this build does not know.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "root": {"type": "string", "description": "Project directory."},
                        "path": {"type": "string", "description": "Diagram file relative to root; defaults to the litz.toml setting or diagram.json."},
                        "parts": {
                            "type": "array",
                            "description": "Parts to add or merge; a matching id updates that part.",
                            "items": {
                                "type": "object",
                                "required": ["id"],
                                "properties": {
                                    "id": {"type": "string"},
                                    "type": {"type": "string"},
                                    "attrs": {"type": "object"}
                                }
                            }
                        },
                        "connections": {
                            "type": "array",
                            "description": "Wires to add as {from,to,color?,route?} or [from,to,color,route].",
                            "items": {"type": ["object", "array"]}
                        }
                    },
                    "required": ["root"],
                    "additionalProperties": false
                }
            },
            {
                "name": "litz_code_generate",
                "description": "Write a starter main.c or main.py and a litz.toml. Nothing is compiled or sent to a cloud service.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "root": {"type": "string", "description": "Project directory."},
                        "language": {"type": "string", "enum": ["c", "python"], "description": "Source language.", "default": "c"},
                        "firmware": {"type": "string", "description": "Firmware path recorded in litz.toml."},
                        "force": {"type": "boolean", "description": "Replace existing starter files.", "default": false}
                    },
                    "required": ["root"],
                    "additionalProperties": false
                }
            },
            {
                "name": "litz_import_diagram",
                "description": "Read a diagram.json and report unknown parts instead of dropping them; optionally write a normalised copy.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "root": {"type": "string", "description": "Project directory."},
                        "source": {"type": "string", "description": "Diagram path relative to root."},
                        "output": {"type": "string", "description": "Where to write a normalised copy, relative to root; omit to only report."}
                    },
                    "required": ["root", "source"],
                    "additionalProperties": false
                }
            }
        ]
    })
}

pub fn is_known(name: &str) -> bool {
    matches!(
        name,
        "litz_check"
            | "litz_test"
            | "litz_run"
            | "litz_circuit_create"
            | "litz_circuit_update"
            | "litz_code_generate"
            | "litz_import_diagram"
    )
}

pub fn call(name: &str, args: &Value) -> Result<Value, String> {
    match name {
        "litz_check" => check(args),
        "litz_test" => test(args),
        "litz_run" => run(args),
        "litz_circuit_create" => circuit_create(args),
        "litz_circuit_update" => circuit_update(args),
        "litz_code_generate" => code_generate(args),
        "litz_import_diagram" => import_diagram(args),
        other => Err(format!("unknown tool {other:?}")),
    }
}

// ------------------------------------------------------------------ helpers

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("missing required string argument {key:?}"))
}

fn optional_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

fn flag(args: &Value, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn open_root(args: &Value) -> Result<ProjectRoot, String> {
    ProjectRoot::open(required_str(args, "root")?)
}

/// The builtin catalog plus a project's own `parts/` and `boards/` directories.
fn registry(root: &ProjectRoot) -> Result<Registry, String> {
    let mut registry = Registry::builtin();
    for sub in ["parts", "boards"] {
        let dir = root.path().join(sub);
        if dir.is_dir() {
            let canonical = dir.canonicalize().map_err(|e| format!("{sub}/: {e}"))?;
            if !canonical.starts_with(root.path()) {
                return Err(format!("{sub}/ escapes the project root"));
            }
            registry.load_dir(&canonical).map_err(|e| e.to_string())?;
        }
    }
    Ok(registry)
}

/// The diagram a project names, or `diagram.json`.
fn diagram_rel(root: &ProjectRoot, override_rel: Option<&str>) -> Result<String, String> {
    if let Some(rel) = override_rel {
        return Ok(rel.to_string());
    }
    let manifest = root.path().join("litz.toml");
    if manifest.is_file() {
        let text =
            fs::read_to_string(&manifest).map_err(|e| format!("{}: {e}", manifest.display()))?;
        if let Ok(value) = toml::from_str::<toml::Value>(&text)
            && let Some(diagram) = value
                .get("project")
                .and_then(|project| project.get("diagram"))
                .and_then(toml::Value::as_str)
        {
            return Ok(diagram.to_string());
        }
    }
    Ok("diagram.json".to_string())
}

/// Check the paths `litz.toml` names before the runner opens any of them.
///
/// The runner resolves firmware, diagram, ROM and script paths itself, so a
/// manifest with `firmware = "../../secret"` would reach outside the sandbox
/// even though every tool argument is confined. This runs the same check on
/// the manifest's paths that the tools run on theirs.
fn preflight(root: &ProjectRoot) -> Result<(), String> {
    let manifest = root.path().join("litz.toml");
    if !manifest.is_file() {
        return Ok(());
    }
    let text = fs::read_to_string(&manifest).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let value: toml::Value =
        toml::from_str(&text).map_err(|e| format!("{}: {e}", manifest.display()))?;
    for (section, key) in [
        ("project", "diagram"),
        ("sim", "firmware"),
        ("sim", "bootloader"),
        ("sim", "partition_table"),
        ("sim", "rom"),
        ("sim", "script"),
    ] {
        let Some(path) = value
            .get(section)
            .and_then(|table| table.get(key))
            .and_then(toml::Value::as_str)
        else {
            continue;
        };
        root.existing(path)
            .map_err(|error| format!("litz.toml [{section}] {key}: {error}"))?;
    }
    for sub in ["parts", "boards"] {
        let dir = root.path().join(sub);
        if dir.exists() {
            let canonical = dir.canonicalize().map_err(|e| format!("{sub}/: {e}"))?;
            if !canonical.starts_with(root.path()) {
                return Err(format!("{sub}/ escapes the project root"));
            }
        }
    }
    Ok(())
}

fn diagnostic_json(diagnostic: &Diagnostic) -> Value {
    json!({
        "severity": match diagnostic.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        },
        "code": diagnostic.code,
        "message": diagnostic.message,
    })
}

fn diagnostics_json(diagnostics: &[Diagnostic]) -> Value {
    Value::Array(diagnostics.iter().map(diagnostic_json).collect())
}

fn render_diagnostics(diagnostics: &[Diagnostic]) -> String {
    diagnostics
        .iter()
        .map(|diagnostic| {
            format!(
                "{}[{}]: {}\n",
                if diagnostic.severity == Severity::Error {
                    "error"
                } else {
                    "warning"
                },
                diagnostic.code,
                diagnostic.message
            )
        })
        .collect()
}

fn blocking(diagnostics: &[Diagnostic], strict: bool) -> bool {
    diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error || (strict && d.severity == Severity::Warning))
}

fn state_json(state: PartState) -> Value {
    match state {
        PartState::Led { lit } => json!({"kind": "led", "lit": lit}),
        PartState::Button { pressed } => json!({"kind": "button", "pressed": pressed}),
        PartState::Switch { on } => json!({"kind": "switch", "on": on}),
        PartState::Segments { mask } => json!({"kind": "segments", "mask": mask}),
        PartState::Tone { sounding, hz } => json!({"kind": "tone", "sounding": sounding, "hz": hz}),
        PartState::Servo { degrees } => json!({"kind": "servo", "degrees": degrees}),
        PartState::Knob { permille } => json!({"kind": "knob", "permille": permille}),
        PartState::Radio {
            tx_count,
            rx_count,
            last_len,
            ..
        } => json!({
            "kind": "radio",
            "tx_count": tx_count,
            "rx_count": rx_count,
            "last_len": last_len
        }),
        PartState::None => json!({"kind": "none"}),
    }
}

// --------------------------------------------------------------------- tools

fn check(args: &Value) -> Result<Value, String> {
    let root = open_root(args)?;
    let strict = flag(args, "strict");
    let rel = diagram_rel(&root, optional_str(args, "diagram"))?;
    let path = root.existing(&rel)?;
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let circuit = Circuit::from_json(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let registry = registry(&root)?;
    let diagnostics = circuit.validate(&registry);
    Ok(json!({
        "passed": !blocking(&diagnostics, strict),
        "diagram": root.display_relative(&path),
        "diagnostics": diagnostics_json(&diagnostics),
    }))
}

fn test(args: &Value) -> Result<Value, String> {
    let root = open_root(args)?;
    preflight(&root)?;
    let strict = flag(args, "strict");
    let scenario = optional_str(args, "scenario")
        .map(|rel| root.existing(rel))
        .transpose()?;
    let runtime = runtime::runtime()?;
    let report = runtime.test(root.path(), scenario.as_deref(), strict)?;
    let tests: Vec<Value> = report
        .tests
        .iter()
        .map(|entry| {
            json!({
                "name": entry.name,
                "status": if entry.passed { "passed" } else { "failed" },
                "exitCode": entry.exit_code,
                "simTimeMs": entry.sim_ms,
                "output": entry.output,
            })
        })
        .collect();
    Ok(json!({
        "status": if report.exit_code == 0 { "passed" } else { "failed" },
        "exitCode": report.exit_code,
        "simTimeMs": report.sim_ms,
        "tests": tests,
        "output": report.output,
    }))
}

fn run(args: &Value) -> Result<Value, String> {
    let root = open_root(args)?;
    preflight(&root)?;
    let strict = flag(args, "strict");
    let timeout = optional_str(args, "timeout");
    let runtime = runtime::runtime()?;
    let report = runtime.run(root.path(), timeout, strict)?;
    let mut states = Map::new();
    for (part, state) in report.part_states {
        states.insert(part, state_json(state));
    }
    Ok(json!({
        "status": match report.exit_code {
            0 => "ok",
            42 => "timeout",
            _ => "failed",
        },
        "exitCode": report.exit_code,
        "simTimeMs": report.sim_ms,
        "serial": report.output,
        "partStates": Value::Object(states),
        "faults": report.faults,
    }))
}

fn circuit_create(args: &Value) -> Result<Value, String> {
    let root = open_root(args)?;
    let registry = registry(&root)?;
    let rel = diagram_rel(&root, optional_str(args, "path"))?;
    let target = root.for_write(&rel)?;
    let existed = target.exists();
    if existed && !flag(args, "force") {
        return Err(format!(
            "{} already exists; pass force: true to replace it",
            root.display_relative(&target)
        ));
    }
    let parts = args
        .get("parts")
        .and_then(Value::as_array)
        .ok_or("missing required array argument \"parts\"")?;
    let connections = args
        .get("connections")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let (parts_doc, connections_doc) = build_document(parts, connections, &registry)?;
    let document = json!({
        "version": 1,
        "parts": parts_doc,
        "connections": connections_doc,
    });
    let text = serde_json::to_string_pretty(&document).map_err(|e| e.to_string())?;
    let circuit = Circuit::from_json(&text).map_err(|e| e.to_string())?;
    let diagnostics = circuit.validate(&registry);
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return Err(format!(
            "the diagram would not validate:\n{}",
            render_diagnostics(&diagnostics)
        ));
    }
    fs::write(&target, &text).map_err(|e| format!("{}: {e}", target.display()))?;
    Ok(json!({
        "written": root.display_relative(&target),
        "replaced": existed,
        "partCount": parts_doc.len(),
        "connectionCount": connections_doc.len(),
        "diagnostics": diagnostics_json(&diagnostics),
    }))
}

fn circuit_update(args: &Value) -> Result<Value, String> {
    let root = open_root(args)?;
    let registry = registry(&root)?;
    let rel = diagram_rel(&root, optional_str(args, "path"))?;
    let source = root.existing(&rel)?;
    let text = fs::read_to_string(&source).map_err(|e| format!("{}: {e}", source.display()))?;
    let mut document: Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", source.display()))?;
    let object = document
        .as_object_mut()
        .ok_or("the diagram must be a JSON object")?;

    if let Some(parts) = args.get("parts") {
        let parts = parts.as_array().ok_or("\"parts\" must be an array")?;
        let existing = object.entry("parts").or_insert_with(|| json!([]));
        let existing = existing
            .as_array_mut()
            .ok_or("\"parts\" in the diagram must be an array")?;
        for part in parts {
            merge_part(existing, part, &registry)?;
        }
    }
    if let Some(connections) = args.get("connections") {
        let connections = connections
            .as_array()
            .ok_or("\"connections\" must be an array")?;
        let existing = object.entry("connections").or_insert_with(|| json!([]));
        let existing = existing
            .as_array_mut()
            .ok_or("\"connections\" in the diagram must be an array")?;
        merge_connections(existing, connections)?;
    }

    let text = serde_json::to_string_pretty(&document).map_err(|e| e.to_string())?;
    let circuit = Circuit::from_json(&text).map_err(|e| e.to_string())?;
    let diagnostics = circuit.check_structure();
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return Err(format!(
            "the merged diagram has structural errors:\n{}",
            render_diagnostics(&diagnostics)
        ));
    }
    let target = root.for_write(&rel)?;
    fs::write(&target, &text).map_err(|e| format!("{}: {e}", target.display()))?;
    Ok(json!({
        "written": root.display_relative(&target),
        "partCount": circuit.parts.len(),
        "connectionCount": circuit.connections.len(),
        "diagnostics": diagnostics_json(&diagnostics),
    }))
}

fn code_generate(args: &Value) -> Result<Value, String> {
    let root = open_root(args)?;
    let language = optional_str(args, "language").unwrap_or("c");
    let force = flag(args, "force");
    let firmware = optional_str(args, "firmware");
    let (source_name, source_body, manifest) = match language {
        "c" => (
            "main.c",
            C_MAIN,
            manifest_c(firmware.unwrap_or("firmware/main.bin")),
        ),
        "python" | "micropython" => (
            "main.py",
            PY_MAIN,
            manifest_python(firmware.unwrap_or("firmware/micropython.bin")),
        ),
        other => return Err(format!("language {other:?} is not \"c\" or \"python\"")),
    };

    let mut written = Vec::new();
    let mut skipped = Vec::new();
    for (name, body) in [
        (source_name.to_string(), source_body.to_string()),
        ("litz.toml".to_string(), manifest),
    ] {
        let target = root.for_write(&name)?;
        if target.exists() && !force {
            skipped.push(name.to_string());
            continue;
        }
        fs::write(&target, body).map_err(|e| format!("{}: {e}", target.display()))?;
        written.push(name.to_string());
    }
    Ok(json!({
        "language": language,
        "written": written,
        "skipped": skipped,
    }))
}

fn import_diagram(args: &Value) -> Result<Value, String> {
    let root = open_root(args)?;
    let source = root.existing(required_str(args, "source")?)?;
    let text = fs::read_to_string(&source).map_err(|e| format!("{}: {e}", source.display()))?;
    let circuit = Circuit::from_json(&text).map_err(|e| format!("{}: {e}", source.display()))?;
    let registry = registry(&root)?;
    let diagnostics = circuit.validate(&registry);

    let mut unknown_parts = Vec::new();
    let mut unknown_attributes = Vec::new();
    for part in &circuit.parts {
        match registry.get(&part.type_key) {
            None => unknown_parts.push(json!({"id": part.id, "type": part.type_key})),
            Some(definition) => {
                for name in part.attrs.keys() {
                    if definition.attr(name).is_none() {
                        unknown_attributes.push(json!({"part": part.id, "attribute": name}));
                    }
                }
            }
        }
    }

    let output = optional_str(args, "output")
        .map(|rel| -> Result<Value, String> {
            let target = root.for_write(rel)?;
            let document: Value =
                serde_json::from_str(&text).map_err(|e| format!("{}: {e}", source.display()))?;
            let pretty = serde_json::to_string_pretty(&document).map_err(|e| e.to_string())?;
            fs::write(&target, pretty).map_err(|e| format!("{}: {e}", target.display()))?;
            Ok(json!(root.display_relative(&target)))
        })
        .transpose()?;

    Ok(json!({
        "source": root.display_relative(&source),
        "output": output,
        "partCount": circuit.parts.len(),
        "connectionCount": circuit.connections.len(),
        "unknownParts": unknown_parts,
        "unknownAttributes": unknown_attributes,
        "diagnostics": diagnostics_json(&diagnostics),
    }))
}

// ------------------------------------------------------------ diagram edits

fn build_document(
    parts: &[Value],
    connections: &[Value],
    registry: &Registry,
) -> Result<(Vec<Value>, Vec<Value>), String> {
    let mut ids = BTreeSet::new();
    let mut parts_doc = Vec::with_capacity(parts.len());
    for part in parts {
        let (id, value) = normalize_part(part, registry)?;
        if !ids.insert(id.clone()) {
            return Err(format!("part id {id:?} is used more than once"));
        }
        parts_doc.push(value);
    }
    let mut connections_doc = Vec::with_capacity(connections.len());
    for connection in connections {
        connections_doc.push(normalize_connection(connection)?);
    }
    Ok((parts_doc, connections_doc))
}

fn normalize_part(part: &Value, registry: &Registry) -> Result<(String, Value), String> {
    let object = part.as_object().ok_or("each part must be an object")?;
    let id = object
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or("each part needs a non-empty string \"id\"")?;
    if id.starts_with('$') {
        return Err(format!("part id {id:?} starts with '$', which is reserved"));
    }
    let type_key = object
        .get("type")
        .and_then(Value::as_str)
        .filter(|key| !key.trim().is_empty())
        .ok_or_else(|| format!("part {id:?} needs a non-empty string \"type\""))?;
    if registry.get(type_key).is_none() {
        return Err(format!(
            "part {id:?} names unknown catalog type {type_key:?}"
        ));
    }
    if object.get("attrs").is_some_and(|attrs| !attrs.is_object()) {
        return Err(format!("part {id:?}: \"attrs\" must be an object"));
    }
    let mut value = part.clone();
    if let Some(map) = value.as_object_mut() {
        map.entry("attrs".to_string()).or_insert_with(|| json!({}));
    }
    Ok((id.to_string(), value))
}

fn merge_part(existing: &mut Vec<Value>, part: &Value, registry: &Registry) -> Result<(), String> {
    let object = part.as_object().ok_or("each part must be an object")?;
    let id = object
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or("each part needs a non-empty string \"id\"")?;
    if object.get("type").is_some_and(|value| !value.is_string()) {
        return Err(format!("part {id:?}: \"type\" must be a string"));
    }
    let type_key = object.get("type").and_then(Value::as_str);
    if let Some(type_key) = type_key
        && registry.get(type_key).is_none()
    {
        return Err(format!(
            "part {id:?} names unknown catalog type {type_key:?}"
        ));
    }

    let position = existing
        .iter()
        .position(|candidate| candidate.get("id").and_then(Value::as_str) == Some(id));
    let Some(position) = position else {
        existing.push(normalize_part(part, registry)?.1);
        return Ok(());
    };

    let target = existing[position]
        .as_object_mut()
        .ok_or_else(|| format!("part {id:?} in the diagram is not an object"))?;
    if let Some(type_key) = type_key {
        target.insert("type".to_string(), json!(type_key));
    }
    for (key, value) in object {
        if key == "id" {
            continue;
        }
        if key == "attrs" {
            let attributes = target
                .entry("attrs")
                .or_insert_with(|| json!({}))
                .as_object_mut()
                .ok_or_else(|| format!("part {id:?}: \"attrs\" must be an object"))?;
            let incoming = value
                .as_object()
                .ok_or_else(|| format!("part {id:?}: \"attrs\" must be an object"))?;
            for (name, value) in incoming {
                attributes.insert(name.clone(), value.clone());
            }
        } else {
            target.insert(key.clone(), value.clone());
        }
    }
    Ok(())
}

fn normalize_connection(connection: &Value) -> Result<Value, String> {
    let (from, to, color, route) = match connection {
        Value::Array(items) => {
            if !(2..=4).contains(&items.len()) {
                return Err("a connection array is [from, to, colour, route]".into());
            }
            let from = items[0]
                .as_str()
                .ok_or("a connection source must be a string")?;
            let to = items[1]
                .as_str()
                .ok_or("a connection target must be a string")?;
            let color = items.get(2).and_then(Value::as_str).unwrap_or("");
            let route = items.get(3).cloned().unwrap_or_else(|| json!([]));
            if !route.is_array() {
                return Err("a connection route must be an array".into());
            }
            (from.to_string(), to.to_string(), color.to_string(), route)
        }
        Value::Object(map) => {
            let from = map
                .get("from")
                .and_then(Value::as_str)
                .ok_or("a connection needs a string \"from\"")?;
            let to = map
                .get("to")
                .and_then(Value::as_str)
                .ok_or("a connection needs a string \"to\"")?;
            let color = map
                .get("color")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let route = map.get("route").cloned().unwrap_or_else(|| json!([]));
            if !route.is_array() {
                return Err("a connection \"route\" must be an array".into());
            }
            (from.to_string(), to.to_string(), color, route)
        }
        _ => {
            return Err(
                "each connection must be an object {from,to} or an array [from,to,color,route]"
                    .into(),
            );
        }
    };
    from.parse::<PinRef>()
        .map_err(|e| format!("connection source {from:?}: {e}"))?;
    to.parse::<PinRef>()
        .map_err(|e| format!("connection target {to:?}: {e}"))?;
    Ok(json!([from, to, color, route]))
}

fn merge_connections(existing: &mut Vec<Value>, connections: &[Value]) -> Result<(), String> {
    let mut keys: BTreeSet<(String, String)> = existing.iter().filter_map(connection_key).collect();
    for connection in connections {
        let normalized = normalize_connection(connection)?;
        if let Some(key) = connection_key(&normalized)
            && keys.insert(key)
        {
            existing.push(normalized);
        }
    }
    Ok(())
}

fn connection_key(connection: &Value) -> Option<(String, String)> {
    let (from, to) = match connection {
        Value::Array(items) if items.len() >= 2 => (items[0].as_str()?, items[1].as_str()?),
        Value::Object(map) => (map.get("from")?.as_str()?, map.get("to")?.as_str()?),
        _ => return None,
    };
    let from = from.parse::<PinRef>().ok()?;
    let to = to.parse::<PinRef>().ok()?;
    Some(if from <= to {
        (from.to_string(), to.to_string())
    } else {
        (to.to_string(), from.to_string())
    })
}

// --------------------------------------------------------------- templates

const C_MAIN: &str = r#"#include <stdio.h>
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "driver/gpio.h"

#define LED_GPIO 8

void app_main(void)
{
    gpio_reset_pin(LED_GPIO);
    gpio_set_direction(LED_GPIO, GPIO_MODE_OUTPUT);
    printf("litz: hello from main.c\n");
    while (true) {
        gpio_set_level(LED_GPIO, 1);
        vTaskDelay(pdMS_TO_TICKS(500));
        gpio_set_level(LED_GPIO, 0);
        vTaskDelay(pdMS_TO_TICKS(500));
    }
}
"#;

const PY_MAIN: &str = r#"import time

print("litz: hello from main.py")
while True:
    print("tick")
    time.sleep(1)
"#;

fn manifest_c(firmware: &str) -> String {
    format!(
        r#"[project]
diagram = "diagram.json"

[sim]
firmware = "{firmware}"
flash_size = "4MB"

[[test]]
name = "the firmware boots"
timeout = "2s"
expect = "litz: hello"
"#
    )
}

fn manifest_python(firmware: &str) -> String {
    format!(
        r#"[project]
diagram = "diagram.json"

[sim]
firmware = "{firmware}"
flash_size = "4MB"
runtime = "micropython"
script = "main.py"

[[test]]
name = "the script runs"
timeout = "5s"
expect = "litz: hello"
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connection_can_be_an_object_or_an_array() {
        let object = normalize_connection(&json!({"from": "esp:GPIO8", "to": "r1:1"})).unwrap();
        let array = normalize_connection(&json!(["esp:GPIO8", "r1:1", "green", []])).unwrap();
        assert_eq!(object[0], "esp:GPIO8");
        assert_eq!(object[2], "");
        assert_eq!(array[2], "green");
    }

    #[test]
    fn a_malformed_connection_is_named() {
        let error = normalize_connection(&json!({"from": "esp"})).unwrap_err();
        assert!(error.contains("\"to\""), "{error}");
        let error = normalize_connection(&json!(["esp", "r1:1"])).unwrap_err();
        assert!(error.contains("source"), "{error}");
    }

    #[test]
    fn unknown_part_types_are_refused_at_creation() {
        let registry = Registry::builtin();
        let error =
            normalize_part(&json!({"id": "x", "type": "litz-nothing"}), &registry).unwrap_err();
        assert!(error.contains("unknown catalog type"), "{error}");
    }

    #[test]
    fn a_manifest_path_cannot_escape_the_root() {
        let dir = std::env::temp_dir().join("litz-mcp-preflight-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        fs::write(
            dir.join("litz.toml"),
            "[sim]\nfirmware = \"../outside.bin\"\n",
        )
        .expect("manifest");
        let root = ProjectRoot::open(&dir.to_string_lossy()).expect("project root");
        let error = preflight(&root).expect_err("the firmware path escapes");
        assert!(error.contains("escapes the project root"), "{error}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn merging_the_same_wire_twice_keeps_one() {
        let mut existing = vec![json!(["esp:GPIO8", "led1:A", "", []])];
        merge_connections(
            &mut existing,
            &[json!({"from": "led1:A", "to": "esp:GPIO8", "color": "green"})],
        )
        .unwrap();
        assert_eq!(existing.len(), 1);
    }
}
