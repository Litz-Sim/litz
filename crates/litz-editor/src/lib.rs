//! The editing model behind any Litz frontend.
//!
//! This crate owns the document — parts, wires and the undo history — and
//! knows nothing about pixels, DOM or a particular UI toolkit. A browser
//! canvas, a native window and a test can all drive the same operations.
//!
//! Every mutation goes through [`Editor::transact`]: the new circuit is built
//! off to the side, validated, and only then committed. A refused edit leaves
//! the document and its history untouched, which is what an undo stack needs
//! to stay honest.
//!
//! Geometry is presentation only. [`route_wire`] produces an orthogonal path
//! for drawing; nothing here feeds a simulation.

#![forbid(unsafe_code)]

use litz_diagram::{Circuit, Connection, Diagnostic, PartInstance, PinRef};
use std::collections::BTreeMap;

/// The grid a wire and a part snap to, in canvas units.
pub const GRID: f64 = 20.0;

/// Why an edit was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    #[error("part id {0:?} is already in use")]
    DuplicateId(String),
    #[error("part id is empty")]
    EmptyId,
    #[error("part {0:?} is not in the document")]
    UnknownPart(String),
    #[error("both ends of a wire are the same pin")]
    SamePin,
    #[error("that wire is already drawn")]
    DuplicateWire,
    #[error("wire {0} does not exist")]
    NoSuchWire(usize),
    #[error("{0}")]
    Invalid(String),
}

/// A point on the canvas.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// Snap a coordinate to the nearest grid intersection.
pub fn snap(value: f64) -> f64 {
    (value / GRID).round() * GRID
}

/// An orthogonal path from `from` to `to`, snapped to the grid.
///
/// Two segments, horizontal first. A wire that already shares a row or column
/// stays a single segment.
pub fn route_wire(from: Point, to: Point) -> Vec<Point> {
    let a = Point::new(snap(from.x), snap(from.y));
    let b = Point::new(snap(to.x), snap(to.y));
    if a.x == b.x || a.y == b.y {
        return vec![a, b];
    }
    vec![a, Point::new(b.x, a.y), b]
}

/// One editable circuit plus its undo and redo history.
#[derive(Debug, Clone)]
pub struct Editor {
    circuit: Circuit,
    undo: Vec<Circuit>,
    redo: Vec<Circuit>,
    dirty: bool,
}

impl Editor {
    /// Wrap an already-parsed circuit. The document starts clean and with an
    /// empty history.
    pub fn new(circuit: Circuit) -> Self {
        Self {
            circuit,
            undo: Vec::new(),
            redo: Vec::new(),
            dirty: false,
        }
    }

    /// An empty document.
    pub fn blank() -> Self {
        Self::new(Circuit {
            version: 1,
            author: None,
            editor: None,
            parts: Vec::new(),
            connections: Vec::new(),
            extra: BTreeMap::new(),
        })
    }

    pub fn circuit(&self) -> &Circuit {
        &self.circuit
    }

    /// Whether there are changes since the last [`Editor::mark_saved`].
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn mark_saved(&mut self) {
        self.dirty = false;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Apply `edit` to a copy of the circuit and commit it only if it returns
    /// Ok. One call is one undo step.
    pub fn transact<T>(
        &mut self,
        edit: impl FnOnce(&mut Circuit) -> Result<T, EditError>,
    ) -> Result<T, EditError> {
        let mut next = self.circuit.clone();
        let value = edit(&mut next)?;
        self.undo.push(std::mem::replace(&mut self.circuit, next));
        self.redo.clear();
        self.dirty = true;
        Ok(value)
    }

    /// Undo the last committed edit. False when the history is empty.
    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.pop() else {
            return false;
        };
        self.redo
            .push(std::mem::replace(&mut self.circuit, previous));
        self.dirty = true;
        true
    }

    /// Re-apply the last undone edit. False when there is nothing to redo.
    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.undo.push(std::mem::replace(&mut self.circuit, next));
        self.dirty = true;
        true
    }

    pub fn add_part(&mut self, part: PartInstance) -> Result<(), EditError> {
        if part.id.trim().is_empty() {
            return Err(EditError::EmptyId);
        }
        self.transact(|circuit| {
            if circuit.parts.iter().any(|p| p.id == part.id) {
                return Err(EditError::DuplicateId(part.id.clone()));
            }
            circuit.parts.push(part);
            Ok(())
        })
    }

    /// Remove a part and every wire attached to it. Returns the number of
    /// wires removed as well.
    pub fn remove_part(&mut self, id: &str) -> Result<usize, EditError> {
        let id = id.to_string();
        self.transact(|circuit| {
            let before = circuit.parts.len();
            circuit.parts.retain(|part| part.id != id);
            if circuit.parts.len() == before {
                return Err(EditError::UnknownPart(id.clone()));
            }
            let wires = circuit.connections.len();
            circuit
                .connections
                .retain(|wire| wire.from.part != id && wire.to.part != id);
            Ok(wires - circuit.connections.len())
        })
    }

    /// Move a part to a canvas position, snapped to the grid.
    pub fn move_part(&mut self, id: &str, left: f64, top: f64) -> Result<(), EditError> {
        let id = id.to_string();
        self.transact(|circuit| {
            let part = circuit
                .parts
                .iter_mut()
                .find(|p| p.id == id)
                .ok_or(EditError::UnknownPart(id.clone()))?;
            part.left = Some(snap(left));
            part.top = Some(snap(top));
            Ok(())
        })
    }

    /// Set one attribute, creating it if needed.
    pub fn set_attr(
        &mut self,
        id: &str,
        key: impl Into<String>,
        value: serde_json::Value,
    ) -> Result<(), EditError> {
        let id = id.to_string();
        let key = key.into();
        self.transact(|circuit| {
            let part = circuit
                .parts
                .iter_mut()
                .find(|p| p.id == id)
                .ok_or(EditError::UnknownPart(id.clone()))?;
            part.attrs.insert(key, value);
            Ok(())
        })
    }

    /// Draw a wire. Duplicate pins and duplicate wires are refused.
    pub fn connect(
        &mut self,
        from: PinRef,
        to: PinRef,
        color: Option<String>,
    ) -> Result<(), EditError> {
        if from == to {
            return Err(EditError::SamePin);
        }
        self.transact(|circuit| {
            for id in [&from.part, &to.part] {
                if !id.starts_with('$') && !circuit.parts.iter().any(|p| &p.id == id) {
                    return Err(EditError::UnknownPart(id.clone()));
                }
            }
            if circuit.connections.iter().any(|wire| {
                (wire.from == from && wire.to == to) || (wire.from == to && wire.to == from)
            }) {
                return Err(EditError::DuplicateWire);
            }
            circuit.connections.push(Connection { from, to, color });
            Ok(())
        })
    }

    pub fn disconnect(&mut self, index: usize) -> Result<(), EditError> {
        self.transact(|circuit| {
            if index >= circuit.connections.len() {
                return Err(EditError::NoSuchWire(index));
            }
            circuit.connections.remove(index);
            Ok(())
        })
    }

    /// Structural diagnostics. Electrical checks live in `litz-board`, which
    /// owns the runtime and the catalog.
    pub fn check(&self) -> Vec<Diagnostic> {
        self.circuit.check_structure()
    }

    /// Validate against a catalog: unknown part types, pins that do not exist,
    /// and connections the schema forbids.
    pub fn validate(&self, registry: &litz_catalog::Registry) -> Vec<Diagnostic> {
        self.circuit.validate(registry)
    }

    /// Serialize the document as `diagram.json`.
    pub fn to_json(&self) -> Result<String, EditError> {
        serde_json::to_string_pretty(&self.circuit).map_err(|e| EditError::Invalid(e.to_string()))
    }

    /// Parse a document from `diagram.json`.
    pub fn from_json(text: &str) -> Result<Self, EditError> {
        Circuit::from_json(text)
            .map(Self::new)
            .map_err(|e| EditError::Invalid(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn part(id: &str, kind: &str) -> PartInstance {
        PartInstance::new(id, kind)
    }

    #[test]
    fn adding_a_part_is_one_undo_step() {
        let mut editor = Editor::blank();
        editor.add_part(part("led1", "litz-led")).unwrap();
        assert_eq!(editor.circuit().parts.len(), 1);
        assert!(editor.is_dirty());
        assert!(editor.undo());
        assert!(editor.circuit().parts.is_empty());
        assert!(editor.redo());
        assert_eq!(editor.circuit().parts.len(), 1);
    }

    #[test]
    fn a_refused_edit_leaves_no_history() {
        let mut editor = Editor::blank();
        editor.add_part(part("led1", "litz-led")).unwrap();
        let before = editor.circuit().parts.len();
        assert_eq!(
            editor.add_part(part("led1", "litz-led")),
            Err(EditError::DuplicateId("led1".into()))
        );
        assert_eq!(editor.circuit().parts.len(), before);
        editor.undo();
        assert!(
            editor.circuit().parts.is_empty(),
            "only the accepted add is undone"
        );
    }

    #[test]
    fn removing_a_part_takes_its_wires() {
        let mut editor = Editor::blank();
        editor
            .add_part(part("esp", "board-esp32-c3-devkitm-1"))
            .unwrap();
        editor.add_part(part("led1", "litz-led")).unwrap();
        editor
            .connect(
                PinRef::new("esp", "GPIO8"),
                PinRef::new("led1", "A"),
                Some("green".into()),
            )
            .unwrap();
        assert_eq!(editor.remove_part("led1"), Ok(1));
        assert!(editor.circuit().connections.is_empty());
    }

    #[test]
    fn a_wire_between_one_pin_and_itself_is_refused() {
        let mut editor = Editor::blank();
        editor.add_part(part("led1", "litz-led")).unwrap();
        let pin = PinRef::new("led1", "A");
        assert_eq!(
            editor.connect(pin.clone(), pin, None),
            Err(EditError::SamePin)
        );
    }

    #[test]
    fn moving_a_part_snaps_to_the_grid() {
        let mut editor = Editor::blank();
        editor.add_part(part("led1", "litz-led")).unwrap();
        editor.move_part("led1", 27.0, -13.0).unwrap();
        let part = &editor.circuit().parts[0];
        assert_eq!((part.left, part.top), (Some(20.0), Some(-20.0)));
    }

    #[test]
    fn attributes_are_created_and_replaced() {
        let mut editor = Editor::blank();
        editor.add_part(part("r1", "litz-resistor")).unwrap();
        editor.set_attr("r1", "value", json!("220")).unwrap();
        editor.set_attr("r1", "value", json!("10k")).unwrap();
        assert_eq!(editor.circuit().parts[0].attrs["value"], json!("10k"));
    }

    #[test]
    fn a_wire_routes_orthogonally_on_the_grid() {
        let path = route_wire(Point::new(3.0, 7.0), Point::new(46.0, 60.0));
        assert_eq!(path.len(), 3);
        assert_eq!(path[0], Point::new(0.0, 0.0));
        assert_eq!(path[1], Point::new(40.0, 0.0));
        assert_eq!(path[2], Point::new(40.0, 60.0));
        for pair in path.windows(2) {
            assert!(
                pair[0].x == pair[1].x || pair[0].y == pair[1].y,
                "not orthogonal"
            );
        }
    }

    #[test]
    fn a_valid_circuit_validates_against_the_builtin_catalog() {
        let editor = Editor::from_json(
            r#"{
              "parts": [
                {"id": "esp", "type": "board-esp32-c3-devkitm-1", "attrs": {}},
                {"id": "r1", "type": "litz-resistor", "attrs": {"value": "220"}},
                {"id": "led1", "type": "litz-led", "attrs": {"color": "green"}}
              ],
              "connections": [
                ["esp:GPIO8", "r1:1", "", []],
                ["r1:2", "led1:A", "", []],
                ["led1:C", "esp:GND.1", "", []]
              ]
            }"#,
        )
        .unwrap();
        let diagnostics = editor.validate(&litz_catalog::Registry::builtin());
        assert!(
            diagnostics
                .iter()
                .all(|d| d.severity != litz_diagram::Severity::Error),
            "{diagnostics:?}"
        );
    }

    #[test]
    fn an_unknown_part_is_an_error() {
        let editor = Editor::from_json(
            r#"{"parts":[{"id":"x","type":"litz-not-a-part","attrs":{}}],"connections":[]}"#,
        )
        .unwrap();
        let diagnostics = editor.validate(&litz_catalog::Registry::builtin());
        assert!(
            diagnostics
                .iter()
                .any(|d| d.severity == litz_diagram::Severity::Error)
        );
    }

    #[test]
    fn save_and_open_round_trip() {
        let mut editor = Editor::blank();
        editor
            .add_part(part("esp", "board-esp32-c3-devkitm-1"))
            .unwrap();
        editor.add_part(part("led1", "litz-led")).unwrap();
        editor
            .connect(PinRef::new("esp", "GPIO8"), PinRef::new("led1", "A"), None)
            .unwrap();
        let text = editor.to_json().unwrap();
        let reopened = Editor::from_json(&text).unwrap();
        assert_eq!(reopened.circuit().parts.len(), 2);
        assert_eq!(reopened.circuit().connections, editor.circuit().connections);
        assert!(!reopened.is_dirty());
    }
}
