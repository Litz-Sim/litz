//! Hierarchy to a flat board.
//!
//! A diagram is nested: a board instance stands for an MCU, the parts soldered
//! to it and the header pins the outside world sees. The runtime does not want
//! any of that indirection, so this pass rewrites the whole thing into one
//! list of parts, one list of nets and one binding from chip pin numbers to
//! nets.
//!
//! Naming is fixed by `docs/formats.md` §3.1: child ids get the instance
//! prefix (`esp/led_pwr`), a board header pin with `gpio = n` becomes chip pin
//! `n`, and a fixed header pin reaches that board's own supply (or the shared
//! ground).

use litz_catalog::{
    BoardInfo, BoardPinTarget, BusDecl, FixedNet, PartManifest, Registry, Simulation,
};
use litz_diagram::{AttrValue, Circuit, Diagnostic, PartInstance, PinKind, PinRef, Severity};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use crate::BuildError;

/// One end of a net.
///
/// Chip pins are split across two variants on purpose. [`Endpoint::Chip`] is
/// GPIO `pin` of development board 0 and stays the spelling for the first
/// (and, until now, only) board, so every existing diagram, test and backend
/// call site that names one chip pin keeps compiling and keeps the same net
/// ordering. [`Endpoint::ChipOn`] names a pin of board index 1 or higher: a
/// second board has to be expressible in the flattened circuit before the CLI
/// can choose firmware per board, and one flat net list has to carry both
/// chips. Board 0 never uses `ChipOn`, which is what makes the one-board path
/// byte-for-byte identical to before.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Endpoint {
    /// GPIO `pin` of development board 0.
    Chip(u8),
    /// GPIO `pin` of development board `board`, for board index 1 and up.
    ChipOn { board: u16, pin: u8 },
    /// Pin `pin` of flat part `part`.
    Part { part: u16, pin: u16 },
    /// A global supply rail: `$gnd`, or a `$v33`/`$v5`/`$vin` wire on the
    /// diagram itself, which is an external supply every board can tap.
    Rail(Rail),
    /// The `rail` terminal of development board `board`'s own supply.
    ///
    /// A board's 3V3 is not the next board's 3V3: it is the output of that
    /// board's regulator, and it is live only while that board is powered.
    /// Naming the rail with its board is what lets one board drive another's
    /// VSYS and switch it on. The ground is shared and stays
    /// [`Endpoint::Rail`], because two circuits that meet have to agree on
    /// ground.
    BoardRail { board: u16, rail: Rail },
}

impl Endpoint {
    /// The endpoint for GPIO `pin` of `board`, normalised so board 0 uses
    /// [`Endpoint::Chip`]. Two boards' GPIO 4 are different endpoints.
    pub fn chip(board: u16, pin: u8) -> Self {
        if board == 0 {
            Endpoint::Chip(pin)
        } else {
            Endpoint::ChipOn { board, pin }
        }
    }

    /// The board index and GPIO a chip endpoint names; `None` for a part pin
    /// or a rail. Board 0 is reported as `(0, pin)`.
    pub fn chip_pin(self) -> Option<(u16, u8)> {
        match self {
            Endpoint::Chip(pin) => Some((0, pin)),
            Endpoint::ChipOn { board, pin } => Some((board, pin)),
            Endpoint::Part { .. } | Endpoint::Rail(_) | Endpoint::BoardRail { .. } => None,
        }
    }

    /// The rail a supply endpoint names, with its board for a board rail and
    /// `None` for the global one.
    pub fn rail_endpoint(self) -> Option<(Option<u16>, Rail)> {
        match self {
            Endpoint::Rail(rail) => Some((None, rail)),
            Endpoint::BoardRail { board, rail } => Some((Some(board), rail)),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rail {
    Gnd,
    V33,
    V5,
    Vin,
}

impl Rail {
    pub fn level(self) -> litz_core::Level {
        match self {
            Rail::Gnd => litz_core::Level::Low,
            _ => litz_core::Level::High,
        }
    }

    pub fn from_pseudo(name: &str) -> Option<Self> {
        match name {
            "$gnd" => Some(Rail::Gnd),
            // `$v3` is the Pico schematic's spelling of the same regulator
            // output; treating it as `$v33` keeps that board's power LED wired.
            "$v3" | "$v33" => Some(Rail::V33),
            "$v5" => Some(Rail::V5),
            "$vin" => Some(Rail::Vin),
            _ => None,
        }
    }

    /// The short name a host and a waveform use for this rail.
    pub fn name(self) -> &'static str {
        match self {
            Rail::Gnd => "gnd",
            Rail::V33 => "v33",
            Rail::V5 => "v5",
            Rail::Vin => "vin",
        }
    }

    /// The name printed on a board header (`3V3`, `VSYS`, `GND`).
    pub fn header_name(self) -> &'static str {
        match self {
            Rail::Gnd => "GND",
            Rail::V33 => "3V3",
            Rail::V5 => "5V",
            Rail::Vin => "VIN",
        }
    }

    fn from_fixed(fixed: FixedNet) -> Self {
        match fixed {
            FixedNet::Gnd => Rail::Gnd,
            FixedNet::V33 => Rail::V33,
            FixedNet::V5 => Rail::V5,
            FixedNet::Vin => Rail::Vin,
        }
    }
}

/// Which behaviour drives a part.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Engine {
    Builtin(String),
    Wasm(WasmModule),
}

/// Where a wasm part's module is, as far as the runtime needs to know.
///
/// The catalog already refused an absolute path or one containing `..`, so
/// this only records two resolved sources: `builtin` is the module compiled
/// into the binary for a builtin catalog entry, and `dir` is the directory of
/// the manifest that named the module for a catalog loaded from disk. Exactly
/// one is set for a well-formed manifest; `parts::build` reads the file when
/// it is the latter.
#[derive(Clone, PartialEq, Eq)]
pub struct WasmModule {
    /// The name as written in the manifest, for error messages.
    pub module: String,
    pub dir: Option<PathBuf>,
    pub builtin: Option<&'static [u8]>,
}

impl fmt::Debug for WasmModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print the module bytes: a `FlatPart` is debug-printed in
        // failures, and a few tens of kilobytes of wasm is not a message.
        f.debug_struct("WasmModule")
            .field("module", &self.module)
            .field("dir", &self.dir)
            .field("builtin", &self.builtin.is_some())
            .finish()
    }
}

/// A leaf part after expansion.
#[derive(Clone, Debug)]
pub struct FlatPart {
    /// `led1` for a top-level part, `esp/led_pwr` for one inside a board.
    pub id: String,
    pub type_key: String,
    pub engine: Engine,
    /// Pin names in the order the catalog declares them; an index into this is
    /// the `PinId` the runtime uses.
    pub pins: Vec<String>,
    /// Each pin's kind, parallel to [`FlatPart::pins`]. Flatten reads the bus
    /// kinds (`i2c-sda`, `spi-cs`, …) to find the wires that carry a bus.
    pub pin_kinds: Vec<PinKind>,
    /// The catalog's attributes with its defaults filled in, as text: a part
    /// engine reads what it needs and ignores the rest.
    pub attrs: BTreeMap<String, String>,
    /// The bus this part was declared on, copied from the manifest. The
    /// resolved attachment is [`FlatPart::device`].
    pub buses: Vec<BusDecl>,
    /// Where this part ended up on a bus, once the diagram's wires are known.
    /// `None` for an ordinary pin-level part.
    pub device: Option<Device>,
}

/// A resolved bus attachment for one part.
///
/// `board` is the development board whose chip the bus pins reach: a device
/// soldered to board `b` is on board `b`'s controller, whatever controller
/// number its manifest names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Device {
    I2c {
        board: u16,
        controller: u8,
        address: u8,
    },
    Spi {
        board: u16,
        controller: u8,
        chip_select: u8,
    },
    Uart {
        board: u16,
        port: u8,
    },
}

/// The physical wiring of every bus in the circuit, derived once during
/// flatten.
///
/// Membership has one source: a manifest's `[buses.*]` section says a part is
/// a device, and the diagram's wires say which chip pins carry that bus. This
/// record is the result, so the runtime and an adapter never re-derive it and
/// cannot disagree.
///
/// The key is `(board, controller)`. A device is on the board whose chip its
/// bus pins reach, so two boards may each run an `i2c0` or a `uart0` at the
/// same controller number without their claims meeting: they are different
/// wires on different chips.
#[derive(Clone, Debug, Default)]
pub struct FlatBuses {
    pub i2c: BTreeMap<(u16, u8), I2cBus>,
    pub spi: BTreeMap<(u16, u8), SpiBus>,
    pub uart: BTreeMap<(u16, u8), UartBus>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct I2cBus {
    pub sda: u8,
    pub scl: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpiBus {
    pub mosi: Option<u8>,
    pub miso: Option<u8>,
    pub sck: u8,
    /// Chip-select GPIO to the id of the part selected by it.
    pub chip_selects: BTreeMap<u8, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UartBus {
    /// The chip pin the part's `uart-tx` is wired to: the chip receives here.
    pub chip_rx: u8,
    /// The chip pin the part's `uart-rx` is wired to: the chip transmits here.
    pub chip_tx: u8,
}

/// The whole circuit, flat.
#[derive(Debug)]
pub struct FlatBoard {
    pub parts: Vec<FlatPart>,
    /// Endpoints that are electrically one node.
    pub nets: Vec<Vec<Endpoint>>,
    /// Reverse index: endpoint to net.
    pub endpoint_net: BTreeMap<Endpoint, u16>,
    /// Chip pins that appear in any wire, unioned across boards.
    ///
    /// Kept in this shape for callers that only care whether a GPIO is wired
    /// at all. Use [`FlatBoard::chip_pins_of`] when the board matters, because
    /// two boards' GPIO 4 are different pads that happen to share a number.
    pub chip_pins: BTreeSet<u8>,
    /// The development boards the diagram places, in diagram order. The index
    /// in this vector is the `board` field of [`Endpoint::ChipOn`] and the
    /// board index the runtime's board-aware calls take.
    pub boards: Vec<BoardInfo>,
    /// Each board's instance id in `diagram.json`, parallel to `boards`. A
    /// point probe is written with it (`b:GPIO8`), so the board has to know
    /// the names it was given rather than just how many there are.
    pub board_ids: Vec<String>,
    /// The chip pins wired on each board, keyed by that board index. Board 0
    /// holds the `Endpoint::Chip` pins; every other board holds its `ChipOn`
    /// pins.
    pub chip_pins_of: BTreeMap<u16, BTreeSet<u8>>,
    /// Whether each board's own power switch is on when the run starts, in
    /// board order. A board is still powered by an external supply on its
    /// VSYS or VBUS pin whatever this says; this is only the switch.
    pub board_power: Vec<bool>,
    /// The bus wiring derived from the manifest declarations and the nets.
    pub buses: FlatBuses,
}

impl FlatBoard {
    /// Whether any pad of `board` shares a net with a pad of another board.
    ///
    /// A direct chip-to-chip wire is how two boards talk UART without a part
    /// on the bus. A backend that answers this has to watch its UART ports
    /// even when no part declares `[buses.uartN]`.
    pub fn has_chip_to_chip_wire(&self, board: u16) -> bool {
        self.chip_pins_of
            .get(&board)
            .into_iter()
            .flatten()
            .any(|&pin| {
                let Some(&net) = self.endpoint_net.get(&Endpoint::chip(board, pin)) else {
                    return false;
                };
                self.nets[usize::from(net)]
                    .iter()
                    .any(|endpoint| matches!(endpoint.chip_pin(), Some((b, _)) if b != board))
            })
    }

    pub fn build(circuit: &Circuit, registry: &Registry) -> Result<Self, BuildError> {
        let diagnostics = circuit.validate(registry);
        let errors: Vec<_> = diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        if !errors.is_empty() {
            return Err(BuildError::Diagnostics(
                errors.into_iter().cloned().collect(),
            ));
        }

        let mut builder = Builder {
            registry,
            parts: Vec::new(),
            pin_index: Vec::new(),
            edges: Vec::new(),
        };

        // Every development board gets an index in the order the diagram
        // places it. That index is what `Endpoint::ChipOn` names, and it is
        // the only thing that keeps two boards' GPIO 4 apart. Board 0 keeps
        // the older `Endpoint::Chip` spelling, so a one-board diagram produces
        // exactly the endpoints, nets and names it did before.
        let board_instances: Vec<&PartInstance> = circuit
            .parts
            .iter()
            .filter(|p| {
                registry
                    .get(&p.type_key)
                    .and_then(|m| m.board.as_ref())
                    .is_some()
            })
            .collect();
        if board_instances.is_empty() {
            return Err(BuildError::Message(
                "the diagram has no development board to run".into(),
            ));
        }
        let board_index: BTreeMap<String, u16> = board_instances
            .iter()
            .enumerate()
            .map(|(index, part)| (part.id.clone(), index as u16))
            .collect();

        // The catalog's board record per index, so a header pin on any board
        // can be mapped without consulting the registry again.
        let mut boards: Vec<BoardInfo> = Vec::with_capacity(board_instances.len());
        for (index, instance) in board_instances.iter().enumerate() {
            let manifest = registry.get(&instance.type_key).expect("checked above");
            let info = manifest.board.as_ref().expect("checked above");
            boards.push(info.clone());
            builder.expand_board(instance, manifest, index as u16)?;
        }

        for part in &circuit.parts {
            // A board instance was expanded above; it never becomes a leaf.
            if board_index.contains_key(&part.id) {
                continue;
            }
            let Some(manifest) = registry.get(&part.type_key) else {
                continue;
            };
            builder.add_part(part.id.clone(), manifest, &part.attrs)?;
        }

        for connection in &circuit.connections {
            let a = builder.top_endpoint(&connection.from, &board_index, &boards)?;
            let b = builder.top_endpoint(&connection.to, &board_index, &boards)?;
            if let (Some(a), Some(b)) = (a, b) {
                builder.edges.push((a, b));
            }
        }

        // A board's supply pins exist whether or not the diagram wires them:
        // the runtime drives the board's own 3V3 from its regulator, senses
        // VSYS/VBUS for an external supply, and a scope can watch any of them.
        // The global ground is seeded for the same reason. These are the
        // last-sorting endpoints, so a circuit that never names a rail keeps
        // every net number it had.
        for board in 0..boards.len() as u16 {
            for rail in [Rail::V33, Rail::V5, Rail::Vin] {
                let endpoint = Endpoint::BoardRail { board, rail };
                builder.edges.push((endpoint, endpoint));
            }
        }
        builder
            .edges
            .push((Endpoint::Rail(Rail::Gnd), Endpoint::Rail(Rail::Gnd)));

        let board_power: Vec<bool> = board_instances
            .iter()
            .map(|instance| match instance.attrs.get("power") {
                Some(serde_json::Value::Bool(on)) => *on,
                Some(serde_json::Value::String(text)) => !matches!(
                    text.trim().to_ascii_lowercase().as_str(),
                    "off" | "false" | "0" | "no"
                ),
                _ => true,
            })
            .collect();

        let (nets, endpoint_net) = resolve_nets(&builder.edges);
        let board_ids: Vec<String> = board_instances
            .iter()
            .map(|instance| instance.id.clone())
            .collect();
        let buses = resolve_buses(&mut builder.parts, &nets, &endpoint_net, &board_ids)?;
        let mut chip_pins: BTreeSet<u8> = BTreeSet::new();
        let mut chip_pins_of: BTreeMap<u16, BTreeSet<u8>> = BTreeMap::new();
        for endpoint in endpoint_net.keys() {
            if let Some((board, pin)) = endpoint.chip_pin() {
                // `chip_pins` stays the union so a caller that only asks
                // "is GPIO 4 wired?" is unchanged; `chip_pins_of` keeps the
                // same pins separated by the board they belong to.
                chip_pins.insert(pin);
                chip_pins_of.entry(board).or_default().insert(pin);
            }
        }
        // A header pin wired only to a host device (the serial monitor) still
        // belongs to its chip; it simply has no board-side net.
        for connection in &circuit.connections {
            for pin in [&connection.from, &connection.to] {
                let Some(&board) = board_index.get(&pin.part) else {
                    continue;
                };
                if let Some(endpoint) = header_endpoint(&boards[board as usize], &pin.name, board)
                    && let Some((_, gpio)) = endpoint.chip_pin()
                {
                    chip_pins.insert(gpio);
                    chip_pins_of.entry(board).or_default().insert(gpio);
                }
            }
        }

        Ok(FlatBoard {
            parts: builder.parts,
            nets,
            endpoint_net,
            chip_pins,
            boards,
            board_ids,
            chip_pins_of,
            board_power,
            buses,
        })
    }

    pub fn part(&self, id: &str) -> Option<&FlatPart> {
        self.parts.iter().find(|p| p.id == id)
    }

    pub fn pin_names(&self, part: u16) -> &[String] {
        &self.parts[part as usize].pins
    }
}

struct Builder<'a> {
    registry: &'a Registry,
    parts: Vec<FlatPart>,
    pin_index: Vec<BTreeMap<String, u16>>,
    edges: Vec<(Endpoint, Endpoint)>,
}

/// The catalog's declared attributes with its defaults, then whatever the
/// diagram wrote on top. Values are text because that is all a part engine
/// needs; validation has already checked each one against its declared type.
fn attributes(
    manifest: &PartManifest,
    given: &BTreeMap<String, serde_json::Value>,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for attr in &manifest.attrs {
        if let Some(default) = &attr.default {
            out.insert(attr.name.clone(), default_text(default));
        }
    }
    for (name, value) in given {
        let text = match value {
            serde_json::Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        out.insert(name.clone(), text);
    }
    out
}

fn default_text(value: &AttrValue) -> String {
    match value {
        AttrValue::Bool(flag) => flag.to_string(),
        AttrValue::Int(number) => number.to_string(),
        AttrValue::Float(number) => number.to_string(),
        AttrValue::String(text) => text.clone(),
    }
}

impl<'a> Builder<'a> {
    fn add_part(
        &mut self,
        id: String,
        manifest: &PartManifest,
        given: &BTreeMap<String, serde_json::Value>,
    ) -> Result<u16, BuildError> {
        let engine = match &manifest.simulation {
            Simulation::Builtin { engine } => Engine::Builtin(engine.clone()),
            Simulation::Wasm { module, .. } => Engine::Wasm(WasmModule {
                module: module.clone(),
                dir: manifest
                    .source
                    .as_deref()
                    .and_then(Path::parent)
                    .map(Path::to_path_buf),
                builtin: self.registry.builtin_part_module(&manifest.id),
            }),
        };
        let pins: Vec<String> = manifest.pins.iter().map(|p| p.name.clone()).collect();
        let pin_kinds: Vec<PinKind> = manifest.pins.iter().map(|p| p.kind).collect();
        let index: BTreeMap<String, u16> = pins
            .iter()
            .enumerate()
            .map(|(i, n)| (n.clone(), i as u16))
            .collect();
        let idx = self.parts.len() as u16;
        self.pin_index.push(index);
        self.parts.push(FlatPart {
            id,
            type_key: manifest.id.clone(),
            engine,
            pins,
            pin_kinds,
            attrs: attributes(manifest, given),
            buses: manifest.buses.clone(),
            device: None,
        });
        Ok(idx)
    }

    fn leaf_endpoint(&self, part: u16, pin: &PinRef) -> Result<Endpoint, BuildError> {
        let Some(&index) = self.pin_index[part as usize].get(&pin.name) else {
            return Err(BuildError::Message(format!(
                "{pin}: {:?} has no pin {:?}",
                self.parts[part as usize].type_key, pin.name
            )));
        };
        Ok(Endpoint::Part { part, pin: index })
    }

    /// Expand a board instance: add its onboard parts under the instance
    /// prefix and turn its internal wires into edges.
    fn expand_board(
        &mut self,
        instance: &PartInstance,
        manifest: &PartManifest,
        board: u16,
    ) -> Result<(), BuildError> {
        let info = manifest.board.as_ref().expect("caller filtered on boards");
        let schematic = load_schematic(manifest, info, self.registry)?;
        let chip_id = schematic
            .parts
            .iter()
            .find(|p| p.type_key == "$chip")
            .map(|p| p.id.clone())
            .ok_or_else(|| {
                BuildError::Message(format!(
                    "board {:?} has no $chip part in its schematic",
                    manifest.id
                ))
            })?;

        let mut children = BTreeMap::new();
        for child in &schematic.parts {
            if child.id == chip_id {
                continue;
            }
            let Some(child_manifest) = self.registry.get(&child.type_key) else {
                return Err(BuildError::Message(format!(
                    "board {:?} uses {:?}, which is not in the catalog",
                    manifest.id, child.type_key
                )));
            };
            // A board may not place another board: its own chip, its own
            // header and the board instance's index are already fixed, and a
            // nested board's pins would have no second index to name them.
            if child_manifest.board.is_some() {
                return Err(BuildError::Message(format!(
                    "board {:?} places {:?}, which is another development board; \
                     a board cannot contain a board",
                    instance.id, child.id
                )));
            }
            let idx = self.add_part(
                format!("{}/{}", instance.id, child.id),
                child_manifest,
                &child.attrs,
            )?;
            children.insert(child.id.clone(), idx);
        }

        for connection in &schematic.connections {
            let a = self.internal_endpoint(&connection.from, &chip_id, &children, info, board)?;
            let b = self.internal_endpoint(&connection.to, &chip_id, &children, info, board)?;
            if let (Some(a), Some(b)) = (a, b) {
                self.edges.push((a, b));
            }
        }
        Ok(())
    }

    fn internal_endpoint(
        &self,
        pin: &PinRef,
        chip_id: &str,
        children: &BTreeMap<String, u16>,
        info: &BoardInfo,
        board: u16,
    ) -> Result<Option<Endpoint>, BuildError> {
        if pin.part == chip_id {
            return Ok(Some(header_endpoint(info, &pin.name, board).ok_or_else(
                || {
                    BuildError::Message(format!(
                        "board schematic wires $chip:{}, which the board does not expose",
                        pin.name
                    ))
                },
            )?));
        }
        if let Some(rail) = Rail::from_pseudo(&pin.part) {
            // The onboard schematic draws its own supply: this board's
            // regulator output is not another board's. Ground stays global.
            return Ok(Some(if rail == Rail::Gnd {
                Endpoint::Rail(rail)
            } else {
                Endpoint::BoardRail { board, rail }
            }));
        }
        if pin.part.starts_with('$') {
            return Ok(None);
        }
        match children.get(&pin.part) {
            Some(&idx) => Ok(Some(self.leaf_endpoint(idx, pin)?)),
            None => Err(BuildError::Message(format!(
                "board schematic wires {:?}, which is not placed in it",
                pin.part
            ))),
        }
    }

    fn top_endpoint(
        &self,
        pin: &PinRef,
        board_index: &BTreeMap<String, u16>,
        boards: &[BoardInfo],
    ) -> Result<Option<Endpoint>, BuildError> {
        if let Some(rail) = Rail::from_pseudo(&pin.part) {
            return Ok(Some(Endpoint::Rail(rail)));
        }
        if pin.part.starts_with('$') {
            return Ok(None);
        }
        // A wire to a board instance names that board's header pin, which is
        // identified by the board index it was expanded under.
        if let Some(&board) = board_index.get(&pin.part) {
            let info = &boards[board as usize];
            return Ok(Some(header_endpoint(info, &pin.name, board).ok_or_else(
                || BuildError::Message(format!("board {:?} has no pin {:?}", pin.part, pin.name)),
            )?));
        }
        let Some(idx) = self.parts.iter().position(|p| p.id == pin.part) else {
            return Err(BuildError::Message(format!(
                "{pin}: {:?} is not placed in the diagram",
                pin.part
            )));
        };
        Ok(Some(self.leaf_endpoint(idx as u16, pin)?))
    }
}

pub(crate) fn header_endpoint(info: &BoardInfo, name: &str, board: u16) -> Option<Endpoint> {
    info.header
        .iter()
        .find(|p| p.name == name)
        .map(|p| match p.target {
            BoardPinTarget::Gpio(n) => Endpoint::chip(board, n as u8),
            BoardPinTarget::Fixed(fixed) => supply_endpoint(board, Rail::from_fixed(fixed)),
        })
}

/// The endpoint for a supply rail seen on a board's header: ground is the one
/// shared node, and every other rail belongs to the board it is printed on.
pub(crate) fn supply_endpoint(board: u16, rail: Rail) -> Endpoint {
    if rail == Rail::Gnd {
        Endpoint::Rail(rail)
    } else {
        Endpoint::BoardRail { board, rail }
    }
}

fn load_schematic(
    manifest: &PartManifest,
    info: &BoardInfo,
    registry: &Registry,
) -> Result<Circuit, BuildError> {
    let text = match (&manifest.source, &info.schematic) {
        (Some(source), Some(name)) => {
            let dir = source.parent().ok_or_else(|| {
                BuildError::Message(format!("board {:?} has no parent directory", manifest.id))
            })?;
            std::fs::read_to_string(dir.join(name)).map_err(|e| {
                BuildError::Message(format!("board {:?} schematic {name}: {e}", manifest.id))
            })?
        }
        _ => registry
            .builtin_schematic(&manifest.id)
            .ok_or_else(|| {
                BuildError::Message(format!(
                    "board {:?} has no schematic; builtin boards only are supported so far",
                    manifest.id
                ))
            })?
            .to_string(),
    };
    Ok(Circuit::from_json(&text)?)
}

/// Resolve every `[buses.*]` declaration against the nets.
///
/// A bus pin is the part's end of a wire; the other end has to be a chip pin,
/// and that pin names both the board and the GPIO the bus uses. Two
/// declarations that disagree about the same controller's pins, that claim one
/// pin for two roles, or that put two devices on one address, chip-select or
/// UART port are ambiguous wiring and are refused before the board is built.
/// Claims are per board, so two boards may each wire their own `i2c0`.
fn resolve_buses(
    parts: &mut [FlatPart],
    nets: &[Vec<Endpoint>],
    endpoint_net: &BTreeMap<Endpoint, u16>,
    board_ids: &[String],
) -> Result<FlatBuses, BuildError> {
    let mut buses = FlatBuses::default();
    let wiring = Wiring {
        nets,
        endpoint_net,
        board_ids,
    };
    // Which role has claimed a `(board, GPIO)`. Two devices on one I2C bus
    // share SDA and SCL, so the same role may be claimed twice; a different
    // role on the same pin is a wiring mistake no manifest can resolve.
    let mut claims: BTreeMap<(u16, u8), String> = BTreeMap::new();
    let mut i2c_addresses: BTreeMap<(u16, u8, u8), String> = BTreeMap::new();

    for (index, part) in parts.iter_mut().enumerate() {
        let decl = match part.buses.as_slice() {
            [] => continue,
            [decl] => decl.clone(),
            many => {
                return Err(BuildError::Message(format!(
                    "part {:?} declares {} buses; a device sits on one bus",
                    part.id,
                    many.len()
                )));
            }
        };
        let index = index as u16;
        let device = match decl {
            BusDecl::I2c {
                controller,
                address,
            } => {
                let (board, sda) = wiring.bus_pin(part, &index, PinKind::I2cSda, "i2c-sda")?;
                let (scl_board, scl) = wiring.bus_pin(part, &index, PinKind::I2cScl, "i2c-scl")?;
                wiring.one_board(
                    part,
                    &format!("i2c{controller}"),
                    &[("SDA", (board, sda)), ("SCL", (scl_board, scl))],
                )?;
                wiring.claim(&mut claims, (board, sda), "i2c{controller} SDA")?;
                wiring.claim(&mut claims, (board, scl), "i2c{controller} SCL")?;
                let key = (board, controller, address);
                if let Some(other) = i2c_addresses.insert(key, part.id.clone()) {
                    return Err(BuildError::Message(format!(
                        "{} i2c{controller} address 0x{address:02x} is claimed by both {other:?} and {:?}",
                        wiring.label(board),
                        part.id
                    )));
                }
                match buses.i2c.get(&(board, controller)) {
                    Some(existing) if existing.sda != sda || existing.scl != scl => {
                        return Err(BuildError::Message(format!(
                            "{} i2c{controller} is wired two ways: SDA GPIO{} / SCL GPIO{} and SDA GPIO{sda} / SCL GPIO{scl} ({:?})",
                            wiring.label(board),
                            existing.sda,
                            existing.scl,
                            part.id
                        )));
                    }
                    Some(_) => {}
                    None => {
                        buses.i2c.insert((board, controller), I2cBus { sda, scl });
                    }
                }
                Device::I2c {
                    board,
                    controller,
                    address,
                }
            }
            BusDecl::Spi {
                controller,
                chip_select,
            } => {
                let mosi = wiring.optional_bus_pin(part, &index, PinKind::SpiMosi, "spi-mosi")?;
                let miso = wiring.optional_bus_pin(part, &index, PinKind::SpiMiso, "spi-miso")?;
                let (sck_board, sck) = wiring.bus_pin(part, &index, PinKind::SpiSck, "spi-sck")?;
                if mosi.is_none() && miso.is_none() {
                    return Err(BuildError::Message(format!(
                        "part {:?} is on spi{controller} but has no wired spi-mosi or spi-miso pin",
                        part.id
                    )));
                }
                let (cs_board, cs) =
                    wiring.named_bus_pin(part, &index, &chip_select, PinKind::SpiCs, "spi-cs")?;
                let mut lines = vec![("SCK", (sck_board, sck)), ("CS", (cs_board, cs))];
                if let Some((board, pin)) = mosi {
                    lines.push(("MOSI", (board, pin)));
                }
                if let Some((board, pin)) = miso {
                    lines.push(("MISO", (board, pin)));
                }
                let board = wiring.one_board(part, &format!("spi{controller}"), &lines)?;
                if let Some((_, pin)) = mosi {
                    wiring.claim(&mut claims, (board, pin), "spi{controller} MOSI")?;
                }
                if let Some((_, pin)) = miso {
                    wiring.claim(&mut claims, (board, pin), "spi{controller} MISO")?;
                }
                wiring.claim(&mut claims, (board, sck), "spi{controller} SCK")?;
                wiring.claim(&mut claims, (board, cs), "spi{controller} CS")?;
                match buses.spi.get_mut(&(board, controller)) {
                    Some(existing) => {
                        let mosi = mosi.map(|(_, pin)| pin);
                        let miso = miso.map(|(_, pin)| pin);
                        merge_spi_line(&part.id, controller, "MOSI", &mut existing.mosi, mosi)?;
                        merge_spi_line(&part.id, controller, "MISO", &mut existing.miso, miso)?;
                        if existing.sck != sck {
                            return Err(BuildError::Message(format!(
                                "{} spi{controller} is wired to two clocks: GPIO{} and GPIO{sck} ({:?})",
                                wiring.label(board),
                                existing.sck,
                                part.id
                            )));
                        }
                        if let Some(other) = existing.chip_selects.insert(cs, part.id.clone()) {
                            return Err(BuildError::Message(format!(
                                "{} spi{controller} chip select GPIO{cs} is shared by {other:?} and {:?}",
                                wiring.label(board),
                                part.id
                            )));
                        }
                    }
                    None => {
                        let mut chip_selects = BTreeMap::new();
                        chip_selects.insert(cs, part.id.clone());
                        buses.spi.insert(
                            (board, controller),
                            SpiBus {
                                mosi: mosi.map(|(_, pin)| pin),
                                miso: miso.map(|(_, pin)| pin),
                                sck,
                                chip_selects,
                            },
                        );
                    }
                }
                Device::Spi {
                    board,
                    controller,
                    chip_select: cs,
                }
            }
            BusDecl::Uart { port } => {
                let (board, chip_rx) = wiring.bus_pin(part, &index, PinKind::UartTx, "uart-tx")?;
                let (tx_board, chip_tx) =
                    wiring.bus_pin(part, &index, PinKind::UartRx, "uart-rx")?;
                wiring.one_board(
                    part,
                    &format!("uart{port}"),
                    &[("TX", (board, chip_rx)), ("RX", (tx_board, chip_tx))],
                )?;
                if chip_rx == chip_tx {
                    return Err(BuildError::Message(format!(
                        "uart{port} wires both directions to GPIO{chip_rx} ({:?})",
                        part.id
                    )));
                }
                wiring.claim(&mut claims, (board, chip_rx), "uart{port} TX")?;
                wiring.claim(&mut claims, (board, chip_tx), "uart{port} RX")?;
                if buses.uart.contains_key(&(board, port)) {
                    return Err(BuildError::Message(format!(
                        "{} uart{port} already has a peer; part {:?} is a second one",
                        wiring.label(board),
                        part.id
                    )));
                }
                buses
                    .uart
                    .insert((board, port), UartBus { chip_rx, chip_tx });
                Device::Uart { board, port }
            }
        };
        part.device = Some(device);
    }

    Ok(buses)
}

/// The nets and board names a bus declaration is resolved against.
struct Wiring<'a> {
    nets: &'a [Vec<Endpoint>],
    endpoint_net: &'a BTreeMap<Endpoint, u16>,
    board_ids: &'a [String],
}

impl Wiring<'_> {
    /// How a board is named in a diagnostic: its diagram id when it has one.
    fn label(&self, board: u16) -> String {
        match self.board_ids.get(board as usize) {
            Some(id) => format!("board {id:?}"),
            None => format!("board {board}"),
        }
    }

    fn claim(
        &self,
        claims: &mut BTreeMap<(u16, u8), String>,
        pin: (u16, u8),
        role: &str,
    ) -> Result<(), BuildError> {
        let (board, gpio) = pin;
        match claims.get(&pin) {
            Some(existing) if existing != role => Err(BuildError::Message(format!(
                "{} GPIO{gpio} is claimed by both {existing} and {role}; a pin carries one bus role",
                self.label(board)
            ))),
            Some(_) => Ok(()),
            None => {
                claims.insert(pin, role.to_string());
                Ok(())
            }
        }
    }

    /// The one board a declaration's pins belong to. A bus whose pins reach
    /// two chips is not a bridge: the same wire on two boards is a mistake in
    /// the diagram, and saying so beats silently attaching the device to one.
    fn one_board(
        &self,
        part: &FlatPart,
        bus: &str,
        pins: &[(&str, (u16, u8))],
    ) -> Result<u16, BuildError> {
        let (first_name, (board, _)) = pins[0];
        if let Some((name, (other, gpio))) = pins.iter().find(|(_, (b, _))| *b != board) {
            return Err(BuildError::Message(format!(
                "part {:?} wires its {bus} {name} to GPIO{gpio} on {}, but its {first_name} is on {}; a bus belongs to one board",
                part.id,
                self.label(*other),
                self.label(board)
            )));
        }
        Ok(board)
    }

    fn bus_pin(
        &self,
        part: &FlatPart,
        index: &u16,
        kind: PinKind,
        pin_name: &str,
    ) -> Result<(u16, u8), BuildError> {
        let pin = pin_of_kind(part, kind, pin_name)?;
        self.wired_chip_pin(*index, part, pin, pin_name)
    }

    fn optional_bus_pin(
        &self,
        part: &FlatPart,
        index: &u16,
        kind: PinKind,
        pin_name: &str,
    ) -> Result<Option<(u16, u8)>, BuildError> {
        if part.pin_kinds.iter().all(|candidate| *candidate != kind) {
            return Ok(None);
        }
        self.bus_pin(part, index, kind, pin_name).map(Some)
    }

    fn named_bus_pin(
        &self,
        part: &FlatPart,
        index: &u16,
        name: &str,
        kind: PinKind,
        pin_name: &str,
    ) -> Result<(u16, u8), BuildError> {
        let pin = part
            .pins
            .iter()
            .position(|candidate| candidate == name)
            .map(|i| i as u16)
            .ok_or_else(|| {
                BuildError::Message(format!(
                    "part {:?} names {name:?} as its chip select but has no such pin",
                    part.id
                ))
            })?;
        if part.pin_kinds.get(pin as usize) != Some(&kind) {
            return Err(BuildError::Message(format!(
                "part {:?} names {name:?} as its chip select, but that pin is not {pin_name}",
                part.id
            )));
        }
        self.wired_chip_pin(*index, part, pin, pin_name)
    }

    /// The board and chip GPIO a part pin is wired to, or an error naming what
    /// is missing.
    fn wired_chip_pin(
        &self,
        part_index: u16,
        part: &FlatPart,
        pin: u16,
        pin_name: &str,
    ) -> Result<(u16, u8), BuildError> {
        let name = part
            .pins
            .get(pin as usize)
            .map(String::as_str)
            .unwrap_or("?");
        let Some(&net) = self.endpoint_net.get(&Endpoint::Part {
            part: part_index,
            pin,
        }) else {
            return Err(BuildError::Message(format!(
                "bus pin {:?}:{name} ({pin_name}) is not wired to the chip",
                part.id
            )));
        };
        let pins: BTreeSet<(u16, u8)> = self.nets[net as usize]
            .iter()
            .filter_map(|endpoint| endpoint.chip_pin())
            .collect();
        let mut pins = pins.into_iter();
        let (board, first) = pins.next().ok_or_else(|| {
            BuildError::Message(format!(
                "bus pin {:?}:{name} ({pin_name}) is not wired to a chip pin",
                part.id
            ))
        })?;
        if let Some((other, second)) = pins.next() {
            return Err(BuildError::Message(format!(
                "bus pin {:?}:{name} ({pin_name}) joins GPIO{first} on {} and GPIO{second} on {}",
                part.id,
                self.label(board),
                self.label(other)
            )));
        }
        Ok((board, first))
    }
}

fn pin_of_kind(part: &FlatPart, kind: PinKind, pin_name: &str) -> Result<u16, BuildError> {
    let mut matches = part
        .pin_kinds
        .iter()
        .enumerate()
        .filter(|(_, candidate)| **candidate == kind)
        .map(|(i, _)| i as u16);
    let first = matches
        .next()
        .ok_or_else(|| BuildError::Message(format!("part {:?} has no {pin_name} pin", part.id)))?;
    if matches.next().is_some() {
        return Err(BuildError::Message(format!(
            "part {:?} has more than one {pin_name} pin",
            part.id
        )));
    }
    Ok(first)
}

fn merge_spi_line(
    part: &str,
    controller: u8,
    line: &str,
    current: &mut Option<u8>,
    found: Option<u8>,
) -> Result<(), BuildError> {
    match (*current, found) {
        (Some(a), Some(b)) if a != b => Err(BuildError::Message(format!(
            "spi{controller} is wired to two {line} lines: GPIO{a} and GPIO{b} ({part:?})"
        ))),
        (None, Some(b)) => {
            *current = Some(b);
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Union every wire into nets. Endpoints are interned in sorted order, so net
/// numbering does not depend on the order a diagram lists its wires.
fn resolve_nets(edges: &[(Endpoint, Endpoint)]) -> (Vec<Vec<Endpoint>>, BTreeMap<Endpoint, u16>) {
    let mut nodes: Vec<Endpoint> = edges.iter().flat_map(|(a, b)| [*a, *b]).collect();
    nodes.sort();
    nodes.dedup();
    let index: BTreeMap<Endpoint, usize> = nodes
        .iter()
        .copied()
        .enumerate()
        .map(|(i, e)| (e, i))
        .collect();

    let mut parent: Vec<usize> = (0..nodes.len()).collect();
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    for (a, b) in edges {
        let (ra, rb) = (find(&mut parent, index[a]), find(&mut parent, index[b]));
        if ra != rb {
            parent[ra.max(rb)] = ra.min(rb);
        }
    }

    let mut groups: BTreeMap<usize, Vec<Endpoint>> = BTreeMap::new();
    for (i, node) in nodes.iter().enumerate() {
        groups.entry(find(&mut parent, i)).or_default().push(*node);
    }
    let mut endpoint_net = BTreeMap::new();
    let mut nets = Vec::with_capacity(groups.len());
    for (i, members) in groups.into_values().enumerate() {
        for member in &members {
            endpoint_net.insert(*member, i as u16);
        }
        nets.push(members);
    }
    (nets, endpoint_net)
}

/// Render diagnostics for an error message.
pub(crate) fn render(diagnostics: &[Diagnostic]) -> String {
    diagnostics
        .iter()
        .map(|d| {
            format!(
                "{} [{}]: {}",
                if d.severity == Severity::Error {
                    "error"
                } else {
                    "warning"
                },
                d.code,
                d.message
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}
