//! The circuit file: `diagram.json`.
//!
//! Nothing here simulates anything. This crate answers three questions:
//! which parts exist, what is wired to what, and whether the wiring is legal
//! against a catalog. Presentation fields (`left`, `top`, `rotate`, `hide`,
//! wire colour and routing) are parsed so a file round-trips, and are then
//! ignored: geometry must never change a simulation result.
//!
//! ```
//! use litz_diagram::{Circuit, Netlist};
//!
//! let json = r#"{
//!   "version": 1,
//!   "parts": [
//!     {"id": "esp", "type": "board-esp32-c3-devkitm-1", "attrs": {}},
//!     {"id": "led1", "type": "litz-led", "attrs": {"color": "green"}}
//!   ],
//!   "connections": [
//!     ["esp:GPIO8", "led1:A", "green", []],
//!     ["led1:C", "esp:GND.1", "black", []]
//!   ]
//! }"#;
//! let circuit = Circuit::from_json(json).unwrap();
//! let nets = Netlist::build(&circuit).unwrap();
//! // led1:A sits on the same net as esp:GPIO8, and nothing else.
//! assert_eq!(nets.nets().len(), 2);
//! ```
#![forbid(unsafe_code)]

pub mod net;
mod pin;

pub use net::{Net, NetId, Netlist};
pub use pin::{
    AttrDef, AttrType, AttrValue, Catalog, EmptyCatalog, PartDef, PinDef, PinKind, PinRef,
    PinRefError, RAILS, nearest,
};

use serde::de::{self, Deserializer};
use serde::ser::{SerializeSeq, Serializer};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// A parsed circuit file.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Circuit {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor: Option<String>,
    #[serde(default)]
    pub parts: Vec<PartInstance>,
    #[serde(default)]
    pub connections: Vec<Connection>,
    /// Keys this crate does not know are kept, not rejected: a file written by
    /// a newer editor must still load.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

fn default_version() -> u32 {
    1
}

/// One placed part.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PartInstance {
    pub id: String,
    #[serde(rename = "type")]
    pub type_key: String,
    #[serde(default)]
    pub attrs: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotate: Option<f64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hide: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl PartInstance {
    pub fn new(id: impl Into<String>, type_key: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            type_key: type_key.into(),
            attrs: BTreeMap::new(),
            left: None,
            top: None,
            rotate: None,
            hide: false,
            extra: BTreeMap::new(),
        }
    }
}

/// One wire: `[from, to, colour, route]` in the file, two pins here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    pub from: PinRef,
    pub to: PinRef,
    /// Presentation only.
    pub color: Option<String>,
}

impl std::fmt::Display for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} -> {}", self.from, self.to)
    }
}

impl Serialize for Connection {
    /// Writes the four-element tuple the file format uses. The routing list is
    /// presentation only and is not modelled, so it is written empty; a file
    /// that had explicit bends keeps them in `extra` only until the editor
    /// grows a route model.
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(4))?;
        seq.serialize_element(&self.from.to_string())?;
        seq.serialize_element(&self.to.to_string())?;
        seq.serialize_element(self.color.as_deref().unwrap_or(""))?;
        seq.serialize_element(&Vec::<serde_json::Value>::new())?;
        seq.end()
    }
}

impl<'de> Deserialize<'de> for Connection {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw: Vec<serde_json::Value> = Vec::deserialize(d)?;
        if raw.len() < 2 {
            return Err(de::Error::custom(
                "a connection needs at least a source and a target pin",
            ));
        }
        let pin = |i: usize| -> Result<PinRef, D::Error> {
            let s = raw[i].as_str().ok_or_else(|| {
                de::Error::custom(format!("connection endpoint {i} is not a string"))
            })?;
            s.parse()
                .map_err(|e: PinRefError| de::Error::custom(e.to_string()))
        };
        let color = raw
            .get(2)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        Ok(Self {
            from: pin(0)?,
            to: pin(1)?,
            color,
        })
    }
}

/// How hard a problem is. `Error` blocks a run; `Warning` does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: &'static str,
    pub message: String,
}

impl Diagnostic {
    fn error(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code,
            message: message.into(),
        }
    }
    fn warning(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            code,
            message: message.into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DiagramError {
    #[error("invalid diagram JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Invalid(String),
}

impl Circuit {
    pub fn new(parts: Vec<PartInstance>, connections: Vec<Connection>) -> Self {
        Self {
            version: 1,
            author: None,
            editor: None,
            parts,
            connections,
            extra: BTreeMap::new(),
        }
    }

    pub fn from_json(text: &str) -> Result<Self, DiagramError> {
        Ok(serde_json::from_str(text)?)
    }

    /// Structural checks that need no catalog: duplicate ids, reserved ids,
    /// wires written twice.
    pub fn check_structure(&self) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        for part in &self.parts {
            if part.id.starts_with('$') {
                out.push(Diagnostic::error(
                    "reserved-id",
                    format!(
                        "part id {:?} starts with '$', which is reserved for host devices",
                        part.id
                    ),
                ));
            }
            if !seen.insert(part.id.as_str()) {
                out.push(Diagnostic::error(
                    "duplicate-id",
                    format!("part id {:?} is used more than once", part.id),
                ));
            }
        }

        let mut wired = BTreeSet::new();
        for c in &self.connections {
            let key = if c.from <= c.to {
                (c.from.clone(), c.to.clone())
            } else {
                (c.to.clone(), c.from.clone())
            };
            if !wired.insert(key) {
                out.push(Diagnostic::warning(
                    "duplicate-wire",
                    format!("{c} appears more than once"),
                ));
            }
        }
        out
    }

    /// Full check against a catalog: known part types, declared pins, legal
    /// pin kinds and attribute types.
    pub fn validate(&self, catalog: &dyn Catalog) -> Vec<Diagnostic> {
        let mut out = self.check_structure();
        let declared: BTreeSet<&str> = self.parts.iter().map(|p| p.id.as_str()).collect();

        for part in &self.parts {
            let Some(def) = catalog.part(&part.type_key) else {
                let hint = nearest(&catalog.ids(), &part.type_key)
                    .map(|guess| format!("; did you mean {guess:?}?"))
                    .unwrap_or_default();
                out.push(Diagnostic::error(
                    "unknown-part",
                    format!(
                        "part {:?} has type {:?}, which is not in the catalog{hint}",
                        part.id, part.type_key
                    ),
                ));
                continue;
            };
            for (name, value) in &part.attrs {
                match def.attr(name) {
                    None => out.push(Diagnostic::warning(
                        "unknown-attr",
                        format!("part {:?} sets unknown attribute {:?}", part.id, name),
                    )),
                    Some(attr) if !attr_matches(attr, value) => {
                        let expected = match attr.kind {
                            AttrType::Enum => format!(
                                "one of {}",
                                attr.values
                                    .iter()
                                    .map(|v| format!("{v:?}"))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                            kind => format!("{kind:?}"),
                        };
                        out.push(Diagnostic::error(
                            "attr-type",
                            format!(
                                "part {:?} attribute {:?} is {}, expected {expected}",
                                part.id,
                                name,
                                json_kind(value),
                            ),
                        ));
                    }
                    Some(_) => {}
                }
            }
        }

        for c in &self.connections {
            self.check_pin(&c.from, catalog, &declared, &mut out);
            self.check_pin(&c.to, catalog, &declared, &mut out);
            // Two rails on one wire is a short circuit, and saying so once beats
            // also reporting that Ground cannot meet Power.
            match (rail_of(&c.from), rail_of(&c.to)) {
                (Some(a), Some(b)) if a != b => out.push(Diagnostic::error(
                    "rail-short",
                    format!("{a} is wired to {b}"),
                )),
                _ => {
                    if let (Some(a), Some(b)) = (
                        self.pin_kind(&c.from, catalog),
                        self.pin_kind(&c.to, catalog),
                    ) && !a.accepts(b)
                    {
                        out.push(Diagnostic::error(
                            "pin-kind",
                            format!("{} ({a:?}) cannot be wired to {} ({b:?})", c.from, c.to),
                        ));
                    }
                }
            }
        }
        out
    }

    fn check_pin(
        &self,
        pin: &PinRef,
        catalog: &dyn Catalog,
        declared: &BTreeSet<&str>,
        out: &mut Vec<Diagnostic>,
    ) {
        if pin.is_pseudo() {
            if !RAILS.contains(&pin.part.as_str()) && !HOST_DEVICES.contains(&pin.part.as_str()) {
                out.push(Diagnostic::error(
                    "unknown-host-device",
                    format!("{pin} names a host device that does not exist"),
                ));
            }
            return;
        }
        let Some(part) = self.parts.iter().find(|p| p.id == pin.part) else {
            let placed: Vec<&str> = declared.iter().copied().collect();
            let hint = nearest(&placed, &pin.part)
                .map(|guess| format!("; did you mean {guess:?}?"))
                .unwrap_or_default();
            out.push(Diagnostic::error(
                "unknown-part-ref",
                if declared.contains(pin.part.as_str()) {
                    format!("{pin} refers to a part that failed to define its type")
                } else {
                    format!(
                        "{pin} refers to {:?}, which is not placed in the diagram{hint}",
                        pin.part
                    )
                },
            ));
            return;
        };
        let Some(def) = catalog.part(&part.type_key) else {
            return;
        };
        match def.pin(&pin.name) {
            None => {
                let hint = nearest(&def.pin_names(), &pin.name)
                    .map(|guess| format!("; did you mean {guess:?}?"))
                    .unwrap_or_default();
                out.push(Diagnostic::error(
                    "unknown-pin",
                    format!("{pin}: {:?} has no pin {:?}{hint}", part.type_key, pin.name),
                ));
            }
            Some(p) if pin.index > p.repeat => out.push(Diagnostic::error(
                "pin-index",
                format!("{pin}: pin {:?} exists {} times", pin.name, p.repeat),
            )),
            Some(_) => {}
        }
    }

    fn pin_kind(&self, pin: &PinRef, catalog: &dyn Catalog) -> Option<PinKind> {
        match pin.part.as_str() {
            "$gnd" => Some(PinKind::Ground),
            "$v33" | "$v5" | "$vin" => Some(PinKind::Power),
            "$serialMonitor" => Some(PinKind::DigitalIo),
            part_id => {
                let part = self.parts.iter().find(|p| p.id == part_id)?;
                catalog.part(&part.type_key)?.pin(&pin.name).map(|p| p.kind)
            }
        }
    }
}

/// Host-side devices a diagram may wire to without declaring them.
pub const HOST_DEVICES: &[&str] = &["$serialMonitor"];

fn rail_of(pin: &PinRef) -> Option<&'static str> {
    if !pin.is_pseudo() {
        return None;
    }
    RAILS.iter().copied().find(|rail| *rail == pin.part)
}

fn attr_matches(def: &AttrDef, value: &serde_json::Value) -> bool {
    match def.kind {
        AttrType::Bool => value.is_boolean(),
        AttrType::Int => value.as_i64().is_some() || value.as_u64().is_some(),
        AttrType::Float => value.is_number(),
        AttrType::String => value.is_string(),
        AttrType::Enum => value
            .as_str()
            .is_some_and(|s| def.values.iter().any(|v| v == s)),
    }
}

fn json_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_diagram() {
        let circuit = Circuit::from_json(
            r#"{
              "version": 1,
              "author": "someone",
              "editor": "litz",
              "parts": [
                {"id": "esp", "type": "board-esp32-c3-devkitm-1", "top": -81.7, "left": 119.8, "attrs": {}},
                {"id": "led1", "type": "litz-led", "attrs": {"color": "green"}},
                {"id": "r1", "type": "litz-resistor", "attrs": {"value": "220"}}
              ],
              "connections": [
                ["esp:GPIO8", "r1:1", "green", []],
                ["r1:2", "led1:A", "green", ["v99"]],
                ["led1:C", "esp:GND.1", "black", ["v99"]]
              ],
              "dependencies": {}
            }"#,
        )
        .unwrap();
        assert_eq!(circuit.parts.len(), 3);
        assert_eq!(circuit.connections[2].from.to_string(), "led1:C");
        assert_eq!(
            circuit.connections[2].to,
            PinRef {
                part: "esp".into(),
                name: "GND".into(),
                index: 1
            }
        );
        assert!(circuit.extra.contains_key("dependencies"));
    }

    #[test]
    fn a_diagram_round_trips_through_json() {
        let source = r#"{
          "version": 1,
          "author": "someone",
          "parts": [
            {"id": "esp", "type": "board-esp32-c3-devkitm-1", "top": -81.7, "left": 119.8, "attrs": {}},
            {"id": "led1", "type": "litz-led", "attrs": {"color": "green"}}
          ],
          "connections": [
            ["esp:GPIO8", "led1:A", "green", []],
            ["led1:C", "esp:GND.1", "black", []]
          ],
          "dependencies": {}
        }"#;
        let circuit = Circuit::from_json(source).unwrap();
        let written = serde_json::to_string(&circuit).unwrap();
        let again = Circuit::from_json(&written).unwrap();
        assert_eq!(again.parts.len(), circuit.parts.len());
        assert_eq!(again.connections, circuit.connections);
        assert_eq!(again.parts[0].left, circuit.parts[0].left);
        assert!(written.contains(r#"["esp:GPIO8","led1:A","green",[]]"#));
        assert!(
            again.extra.contains_key("dependencies"),
            "unknown keys survive a save"
        );
    }

    #[test]
    fn reports_duplicate_ids() {
        let circuit = Circuit::from_json(
            r#"{"parts":[{"id":"a","type":"x"},{"id":"a","type":"y"}],"connections":[]}"#,
        )
        .unwrap();
        let codes: Vec<_> = circuit
            .check_structure()
            .into_iter()
            .map(|d| d.code)
            .collect();
        assert!(codes.contains(&"duplicate-id"));
    }
}
