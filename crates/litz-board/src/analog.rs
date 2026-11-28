//! Analog co-simulation over the board's flat nets.
//!
//! The digital runtime resolves every net to one of three levels. That is the
//! right model for logic, and it is deliberately the only model a digital-only
//! run pays for. This module adds an *optional* view on top of the same nets:
//! each net becomes a node in a [`litz_analog::Netlist`], the parts with an
//! electrical model become elements, and the solver gives every node a real
//! voltage — a DC operating point by default, or a backward-Euler transient
//! when the board is advanced over time.
//!
//! Two mappings are fixed:
//!
//! * **nets to nodes.** One node per net. The net holding `$gnd` is node 0.
//!   `$v33`, `$v5` and `$vin` are ideal sources to ground (3.3 V, 5 V and 5 V
//!   unless the [`AnalogConfig`] says otherwise). A net that no element and
//!   no active driver reaches is left out of the solve entirely, so its
//!   voltage is `None` — high impedance, not 0 V.
//! * **parts to elements.** A resistor is a resistor; an LED is a diode with a
//!   colour-dependent forward voltage plus a small series resistance; a
//!   potentiometer is two resistors with the wiper between them; a capacitor
//!   and an inductor are the corresponding reactive elements; a switch is a
//!   small resistance closed and a large one open; an NTC or photoresistor is
//!   a two-resistor divider whose ratio follows its host control; and the
//!   generic `bjt`, `mosfet` and `opamp` engines become their nonlinear
//!   devices. A part with no electrical model is skipped.
//!
//! Mixed mode is the point of the exercise. A chip pin that drives a net is
//! stamped as a Thevenin source — 3.3 V or 0 V behind a small driver
//! resistance — and a high-impedance pin contributes nothing. The solved
//! voltage on a part's analog pin is then what the chip's ADC reads.
//!
//! A chip pin is identified by `(board, GPIO)`, the same pair the runtime
//! uses, so two development boards' GPIO 4 are distinct nodes even though
//! they share a number. A net that joins the two boards' pins carries one
//! voltage, as a wire between them would.
//!
//! The solve is pure: [`solve`] only reads the flat circuit, the chip's
//! drives and the parts' host-visible state. It never changes the digital
//! runtime, which is why an analog-off board is bit-for-bit the board it was
//! before this module existed.
//!
//! # Transient state
//!
//! [`build`] with `transient = true` connects capacitor terminals for the
//! purpose of deciding which nets are solvable (a capacitor is a path at AC,
//! not at DC). The caller owns the [`litz_analog::Transient`]; [`Circuit`]
//! holds only the net-to-node map, so a new operating point can be extracted
//! from every step without rebuilding the netlist.

use crate::flatten::{Endpoint, Engine, FlatBoard, FlatPart, Rail};
use litz_analog::{
    BjtModel, DiodeModel, Element, ElementId, MosfetModel, Netlist, OpAmpModel,
    Solution as NetlistSolution, THERMAL_VOLTAGE,
};
use litz_core::{Drive, Level, Part, PartState, PinId, Strength};
use std::collections::{BTreeMap, BTreeSet};

/// The smallest resistance the solver is given. A potentiometer at either end
/// of its travel is a short; a tiny resistance keeps the netlist well formed
/// without changing the result at the precision a board run needs.
const MIN_OHMS: f64 = 1e-3;

/// A closed pushbutton or switch contact, in ohms.
const SWITCH_CLOSED_OHMS: f64 = 0.1;

/// An open pushbutton or switch contact, in ohms. Ten megohms is large enough
/// to read as an open circuit against any sane pull resistor and small enough
/// that the node keeps a well-defined DC solution.
const SWITCH_OPEN_OHMS: f64 = 10_000_000.0;

/// Track resistance used for a sensor's output divider when the manifest does
/// not say otherwise. It sets the divider's output impedance, not its ratio.
const SENSOR_OHMS: f64 = 10_000.0;

/// A multimeter in voltage mode stands across two nets without loading them:
/// ten megohms is what a handheld meter presents, and low enough that the
/// node still has a well-defined DC solution.
const VOLTMETER_OHMS: f64 = 10_000_000.0;

/// A multimeter in current mode sits in series, where its shunt is the only
/// thing between the two nets. One hundredth of an ohm keeps the reading
/// within a percent of a short against the loads a board run sees.
const AMMETER_OHMS: f64 = 0.01;

/// The input-to-output voltage a fixed 78xx/79xx regulator needs to hold its
/// output, in volts. Below it the pass transistor is fully on and the output
/// follows the input down.
const REGULATOR_DROPOUT_VOLTS: f64 = 2.0;

/// The same figure for an LM317 at a modest load, in volts. Its dropout is
/// lower than a fixed regulator's Darlington pass stage.
const LM317_DROPOUT_VOLTS: f64 = 1.5;

/// The voltage an LM317 holds between its output and its adjust pin, in
/// volts. The datasheet's 1.25 V reference is what makes a divider set the
/// output: `Vout = 1.25 × (1 + R2/R1)`.
const LM317_REFERENCE_VOLTS: f64 = 1.25;

/// The smallest output an LM317 can regulate to, in volts.
const LM317_MIN_VOLTS: f64 = 1.25;

/// The largest output a host may dial from an LM317, in volts.
const LM317_MAX_VOLTS: f64 = 24.0;

/// Open-loop gain of the op-amp that models the LM317's error amplifier. The
/// finite gain leaves a few microvolts of setpoint error, which is far below
/// what a board solves to.
const LM317_OPAMP_GAIN: f64 = 1e7;

/// The upper rail of that error amplifier, in volts. A regulator cannot make
/// more than its input allows, which the dropout fold enforces; this rail
/// only keeps the op-amp from being clamped by the passive network.
const LM317_RAIL_VOLTS: f64 = 60.0;

/// Voltages and resistances used to turn board parts into elements.
///
/// The defaults describe the dev boards in the built-in catalog: a 3.3 V logic
/// rail, a 5 V USB rail, a 50 Ω chip output and a 10 kΩ potentiometer. A
/// caller that knows better (a 1.8 V part, a higher-impedance source) sets its
/// own with [`crate::Board::set_analog_config`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AnalogConfig {
    /// Voltage of `$v33`, in volts.
    pub v33: f64,
    /// Voltage of `$v5`, in volts.
    pub v5: f64,
    /// Voltage of `$vin` when the diagram does not configure it, in volts.
    pub vin: f64,
    /// Series resistance of a driven chip pin, in ohms.
    pub driver_ohms: f64,
    /// Series resistance bundled into an LED model, in ohms.
    pub led_series_ohms: f64,
    /// Current above which an LED counts as lit, in amperes.
    pub led_lit_amps: f64,
    /// Track resistance of a potentiometer whose manifest does not say one.
    pub pot_ohms: f64,
}

impl Default for AnalogConfig {
    fn default() -> Self {
        Self {
            v33: 3.3,
            v5: 5.0,
            vin: 5.0,
            driver_ohms: 50.0,
            led_series_ohms: 20.0,
            led_lit_amps: 1e-4,
            pot_ohms: 10_000.0,
        }
    }
}

/// A solved operating point, addressed the way a test or an adapter asks.
///
/// Every accessor returns `None` when the net is not part of the solve. That
/// is the high-impedance case: a pin wired to nothing, or to a part that no
/// source or ground path reaches. It is deliberately not `Some(0.0)`.
#[derive(Clone, Debug, Default)]
pub struct AnalogSolution {
    /// Voltage by flat-net id. `None` means the net was not solved.
    net_volts: Vec<Option<f64>>,
    /// Voltage by `(part index, pin index)` for every solved part pin.
    pin_volts: BTreeMap<(u16, u16), f64>,
    /// Voltage by `(board, chip GPIO)`, so two boards' GPIO 4 stay apart.
    gpio_volts: BTreeMap<(u16, u8), f64>,
    /// Current through a part's main element, in amperes.
    part_currents: BTreeMap<u16, f64>,
    /// Parts that carry an LED model, lit or not.
    leds: BTreeSet<u16>,
    /// Parts whose LED is lit.
    lit: BTreeSet<u16>,
    /// Part ids by flat index, so a name can be resolved without the board.
    part_ids: Vec<String>,
    /// Pin names by flat index, parallel to the part list.
    pin_names: Vec<Vec<String>>,
}

impl AnalogSolution {
    /// Voltage on one named pin of a named part, in volts.
    ///
    /// `None` when the part or pin does not exist, or when that node is
    /// floating (not reached by the solve).
    pub fn node_voltage(&self, part: &str, pin: &str) -> Option<f64> {
        let index = self.part_index(part)?;
        let pin = self
            .pin_names
            .get(index as usize)?
            .iter()
            .position(|name| name == pin)? as u16;
        self.node_voltage_pin(index, pin)
    }

    /// Voltage on GPIO `gpio` of board 0, in volts. `None` when the pin is
    /// floating. Board 0 is what a one-board test or adapter asks for.
    pub fn node_voltage_gpio(&self, gpio: u8) -> Option<f64> {
        self.node_voltage_gpio_on(0, gpio)
    }

    /// Voltage on GPIO `gpio` of board `board`, in volts. `None` when the pin
    /// is floating.
    pub fn node_voltage_gpio_on(&self, board: u16, gpio: u8) -> Option<f64> {
        self.gpio_volts.get(&(board, gpio)).copied()
    }

    /// Voltage on a flat net, in volts. `None` when the net was not solved.
    pub fn node_voltage_net(&self, net: u16) -> Option<f64> {
        self.net_volts.get(net as usize).copied().flatten()
    }

    /// Voltage on a part pin by flat indices.
    pub fn node_voltage_pin(&self, part: u16, pin: u16) -> Option<f64> {
        self.pin_volts.get(&(part, pin)).copied()
    }

    /// Whether a named pin is floating: it exists but has no solved voltage.
    /// `None` when the part or pin does not exist at all.
    pub fn node_is_high_z(&self, part: &str, pin: &str) -> Option<bool> {
        let index = self.part_index(part)?;
        let pin = self
            .pin_names
            .get(index as usize)?
            .iter()
            .position(|name| name == pin)? as u16;
        Some(!self.pin_volts.contains_key(&(index, pin)))
    }

    /// Current through a named part's main element, in amperes.
    ///
    /// For an LED this is the forward current through the junction; for a
    /// resistor it is the current from its first pin to its second.
    pub fn current(&self, part: &str) -> Option<f64> {
        let index = self.part_index(part)?;
        self.part_currents.get(&index).copied()
    }

    /// Whether a named part is an LED, and whether it is lit. `None` when the
    /// part is not an LED model.
    pub fn is_lit(&self, part: &str) -> Option<bool> {
        let index = self.part_index(part)?;
        self.leds
            .contains(&index)
            .then(|| self.lit.contains(&index))
    }

    /// The number of nets this solution covers, for a caller that wants to
    /// inspect [`AnalogSolution::node_voltage_net`] over the whole board.
    pub fn net_count(&self) -> usize {
        self.net_volts.len()
    }

    fn part_index(&self, part: &str) -> Option<u16> {
        self.part_ids
            .iter()
            .position(|id| id == part)
            .map(|i| i as u16)
    }
}

/// Solve the board's DC operating point.
///
/// `drives` is the chip's current output state, `parts` is the runtime's part
/// list (the potentiometer's position lives in its [`PartState`]), and
/// `config` supplies the rail and driver values. The result is deterministic:
/// nodes are numbered in net order, elements are added in part order, and the
/// solver itself has no nondeterminism.
pub(crate) fn solve(
    flat: &FlatBoard,
    drives: &BTreeMap<(u16, u8), Drive>,
    pwm: &BTreeMap<(u16, u8), f64>,
    parts: &[Box<dyn Part>],
    config: &AnalogConfig,
    powered: &[bool],
    driven: &BTreeMap<u16, f64>,
) -> Result<AnalogSolution, String> {
    build_operating(flat, drives, pwm, parts, config, false, powered, driven)?.solve_dc(config)
}

/// Build the circuit at its operating point, with every regulated source that
/// is over its current limit folded back to that limit and every regulator
/// that cannot hold its setpoint reduced to what its input allows.
///
/// The first build assumes constant voltage. If the load then draws more than
/// a supply allows, that supply is modelled as a current source and the circuit
/// is built again; folding one supply back can push another over, so the set
/// only grows and the loop ends after at most one build per limiting supply.
/// A regulator whose input is too close to its output is handled the same way:
/// it is rebuilt as a source that tracks the input at the dropout, and the
/// rebuilt circuit may droop another regulator in turn.
///
/// The mode is fixed for the lifetime of the returned circuit, which is what a
/// transient run wants: a load change rebuilds the circuit anyway.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_operating(
    flat: &FlatBoard,
    drives: &BTreeMap<(u16, u8), Drive>,
    pwm: &BTreeMap<(u16, u8), f64>,
    parts: &[Box<dyn Part>],
    config: &AnalogConfig,
    transient: bool,
    powered: &[bool],
    driven: &BTreeMap<u16, f64>,
) -> Result<Circuit, String> {
    let mut limited = BTreeSet::new();
    let mut drooped: BTreeMap<u16, f64> = BTreeMap::new();
    let mut converted: BTreeMap<u16, f64> = BTreeMap::new();
    // One build per folded supply plus one per drooped regulator or converter
    // whose target follows its input; the cap is a backstop against a target
    // that keeps moving with the input it depends on.
    for _ in 0..16 {
        let circuit = build(
            flat, drives, pwm, parts, config, transient, &limited, &drooped, &converted, powered,
            driven,
        )?;
        let solution = circuit.solve_dc(config)?;
        let folded = folded_sources(flat, parts, &solution);
        let grown: BTreeSet<u16> = limited.union(&folded).copied().collect();
        let next = droop_targets(flat, parts, &solution, &drooped);
        let next_converted = converter_targets(flat, parts, &solution, &converted);
        if grown == limited && next == drooped && next_converted == converted {
            return Ok(circuit);
        }
        limited = grown;
        drooped = next;
        converted = next_converted;
    }
    build(
        flat, drives, pwm, parts, config, transient, &limited, &drooped, &converted, powered,
        driven,
    )
}

/// The output each switching converter actually reaches, given the input the
/// circuit just solved it with: a buck can only come down, a boost only up.
fn converter_targets(
    flat: &FlatBoard,
    parts: &[Box<dyn Part>],
    solution: &AnalogSolution,
    converted: &BTreeMap<u16, f64>,
) -> BTreeMap<u16, f64> {
    let mut out = converted.clone();
    for (index, part) in flat.parts.iter().enumerate() {
        if !matches!(&part.engine, Engine::Builtin(engine) if engine == "dc-dc") {
            continue;
        }
        let Some(vin) = solution.node_voltage(&part.id, "VI") else {
            continue;
        };
        let boost = part
            .attrs
            .get("topology")
            .is_some_and(|topology| topology.trim() == "boost");
        // The host can dial the module while the board runs, so the part's
        // live reading wins over the attribute it started with.
        let setpoint = parts
            .get(index)
            .and_then(|part| {
                let pin = flat
                    .parts
                    .get(index)?
                    .pins
                    .iter()
                    .position(|name| name == "VO")? as u16;
                part.analog(PinId(pin))
            })
            .map_or_else(
                || {
                    part.attrs
                        .get("voltage")
                        .and_then(|text| text.trim().parse::<f64>().ok())
                        .unwrap_or(5.0)
                },
                f64::from,
            );
        let target = if boost {
            setpoint.max(vin)
        } else {
            setpoint.min(vin)
        };
        out.insert(index as u16, target.max(0.0));
    }
    out
}

/// The output voltage each regulator that cannot hold its setpoint must show,
/// given the circuit just solved.
///
/// A regulator already in the map keeps its entry: it is a state of the
/// operating point, not a per-iteration judgement, so a load change cannot
/// make the fold oscillate. A regulator becomes drooped the first time its
/// input is closer to the output than the dropout, and its target is then the
/// input minus that dropout (or minus zero when the input is below it).
fn droop_targets(
    flat: &FlatBoard,
    parts: &[Box<dyn Part>],
    solution: &AnalogSolution,
    drooped: &BTreeMap<u16, f64>,
) -> BTreeMap<u16, f64> {
    let mut out = drooped.clone();
    for (index, part) in flat.parts.iter().enumerate() {
        let Engine::Builtin(engine) = &part.engine else {
            continue;
        };
        if engine != "voltage-regulator" {
            continue;
        }
        let index = index as u16;
        let Some((setpoint, negative, dropout)) = regulator_spec(part, Some(index), parts) else {
            continue;
        };
        let Some(vin) = solution.node_voltage(&part.id, "VI") else {
            continue;
        };
        // An adjustable part whose feedback pin is wired sets its own output:
        // the divider decides the setpoint, so the solved VO is what the
        // dropout has to be measured against, not what the host dialed.
        let setpoint = if part
            .attrs
            .get("variant")
            .is_some_and(|variant| variant.trim() == "LM317")
            && adjust_net(flat, index).is_some()
        {
            solution
                .node_voltage(&part.id, "VO")
                .map_or(setpoint, f64::abs)
        } else {
            setpoint
        };
        // The absolute input the pass element sees, on the regulator's side.
        let magnitude = if negative { -vin } else { vin };
        let in_droop = magnitude - setpoint.abs() < dropout - 1e-9;
        if !out.contains_key(&index) && !in_droop {
            continue;
        }
        let target = (magnitude - dropout).max(0.0);
        out.insert(index, if negative { -target } else { target });
    }
    out
}

/// The net a part's `ADJ` pin is wired to, `None` when the pin is unwired or
/// the part has no such pin.
fn adjust_net(flat: &FlatBoard, index: u16) -> Option<usize> {
    let part = flat.parts.get(usize::from(index))?;
    let pin = part.pins.iter().position(|name| name == "ADJ")? as u16;
    flat.endpoint_net
        .get(&Endpoint::Part { part: index, pin })
        .map(|net| *net as usize)
}

/// A regulator's setpoint, polarity and dropout, or `None` when the part is
/// not wired as one.
fn regulator_spec(
    part: &FlatPart,
    index: Option<u16>,
    parts: &[Box<dyn Part>],
) -> Option<(f64, bool, f64)> {
    let variant = part.attrs.get("variant").map_or("7805", String::as_str);
    let (setpoint, negative, dropout) = match variant.trim() {
        "7812" => (12.0, false, REGULATOR_DROPOUT_VOLTS),
        "7905" => (5.0, true, REGULATOR_DROPOUT_VOLTS),
        "LM317" => {
            // A host can dial the adjustable part while the board runs; the
            // attribute is where it starts.
            let live = index
                .and_then(|index| {
                    let pin = part
                        .pins
                        .iter()
                        .position(|name| name == "VO")
                        .map(|pin| pin as u16)?;
                    parts.get(index as usize)?.analog(PinId(pin))
                })
                .map(f64::from);
            let volts = live
                .or_else(|| {
                    part.attrs
                        .get("voltage")
                        .and_then(|text| text.trim().parse::<f64>().ok())
                })
                .unwrap_or(5.0);
            (
                volts.clamp(LM317_MIN_VOLTS, LM317_MAX_VOLTS),
                false,
                LM317_DROPOUT_VOLTS,
            )
        }
        _ => (5.0, false, REGULATOR_DROPOUT_VOLTS),
    };
    Some((setpoint, negative, dropout))
}

/// Supplies whose load draws more than their current limit.
fn folded_sources(
    flat: &FlatBoard,
    parts: &[Box<dyn Part>],
    solution: &AnalogSolution,
) -> BTreeSet<u16> {
    parts
        .iter()
        .enumerate()
        .filter_map(|(index, part)| {
            let limit = part.current_limit()?;
            // `part_currents` follows the element's first-terminal-to-second
            // sense: a source delivering to its load reads negative.
            let delivered = -solution.current(flat.parts.get(index)?.id.as_str())?;
            (delivered > f64::from(limit) + 1e-9).then_some(index as u16)
        })
        .collect()
}

/// Whether an engine is an analog-only element this build maps but does not
/// (yet) have a digital part for.
///
/// `parts::build` rejects an unknown builtin engine. A capacitor, inductor,
/// transistor or op-amp has no digital behaviour at all, so a board may run
/// with one before the builtin engine lands; the analog view reads its
/// attributes directly and only needs the pins to exist. Once the engine is
/// registered, `parts::build` succeeds and this fallback is never reached.
pub(crate) fn is_analog_only_engine(part: &FlatPart) -> bool {
    matches!(
        &part.engine,
        Engine::Builtin(engine)
            if matches!(
                engine.as_str(),
                "capacitor" | "inductor" | "bjt" | "mosfet" | "opamp" | "op-amp"
            )
    )
}

/// The netlist plus everything needed to read a solution back out, built once
/// and reused by every transient step.
///
/// The netlist is static for a run: a chip drive, a press, a control change or
/// a new [`AnalogConfig`] discards the whole circuit (and the transient state
/// with it) rather than patching it, which keeps the numeric history and the
/// topology consistent.
pub(crate) struct Circuit {
    netlist: Netlist,
    /// Node number for a flat net; `None` when the net is floating.
    node_of_net: Vec<Option<usize>>,
    /// The flat board's endpoints, copied so a solved node can be reported on
    /// every pin and chip pad tied to it.
    nets: Vec<Vec<Endpoint>>,
    net_count: usize,
    part_ids: Vec<String>,
    pin_names: Vec<Vec<String>>,
    current_elements: Vec<(u16, ElementId)>,
    leds: BTreeSet<u16>,
}

impl Circuit {
    /// The netlist a transient run steps.
    pub(crate) fn netlist(&self) -> &Netlist {
        &self.netlist
    }

    /// Solve the circuit's DC operating point.
    pub(crate) fn solve_dc(&self, config: &AnalogConfig) -> Result<AnalogSolution, String> {
        if self.netlist.is_empty() {
            return Ok(self.blank());
        }
        let solved = self
            .netlist
            .solve_dc()
            .map_err(|e| format!("analog: the netlist could not be solved: {e}"))?;
        Ok(self.solution(&solved, config))
    }

    /// Turn a solver result into the board-addressed view.
    pub(crate) fn solution(
        &self,
        solved: &NetlistSolution,
        config: &AnalogConfig,
    ) -> AnalogSolution {
        let mut solution = self.blank();
        for (net, node) in self.node_of_net.iter().enumerate() {
            if let Some(node) = *node {
                solution.net_volts[net] = solved.node_volts.get(node).copied();
            }
        }
        self.place_endpoints(&mut solution);
        for &(part, element) in &self.current_elements {
            if let Ok(amps) = solved.probe_current(element) {
                solution.part_currents.insert(part, amps);
                if self.leds.contains(&part) && amps > config.led_lit_amps {
                    solution.lit.insert(part);
                }
            }
        }
        solution.leds.clone_from(&self.leds);
        solution
    }

    /// The `t = 0` solution of a zero-state transient: every solved node is
    /// 0 V until the first step moves it.
    pub(crate) fn initial_solution(&self) -> AnalogSolution {
        let mut solution = self.blank();
        for (net, node) in self.node_of_net.iter().enumerate() {
            if node.is_some() {
                solution.net_volts[net] = Some(0.0);
            }
        }
        self.place_endpoints(&mut solution);
        solution.leds.clone_from(&self.leds);
        solution
    }

    fn blank(&self) -> AnalogSolution {
        AnalogSolution {
            net_volts: vec![None; self.net_count],
            part_ids: self.part_ids.clone(),
            pin_names: self.pin_names.clone(),
            ..AnalogSolution::default()
        }
    }

    /// Copy every solved net voltage onto the chip and part pins on that net.
    fn place_endpoints(&self, solution: &mut AnalogSolution) {
        for (net, members) in self.nets.iter().enumerate() {
            let Some(volts) = solution.net_volts.get(net).copied().flatten() else {
                continue;
            };
            for endpoint in members {
                match *endpoint {
                    Endpoint::Chip(gpio) => {
                        solution.gpio_volts.insert((0, gpio), volts);
                    }
                    Endpoint::ChipOn { board, pin } => {
                        solution.gpio_volts.insert((board, pin), volts);
                    }
                    Endpoint::Part { part, pin } => {
                        solution.pin_volts.insert((part, pin), volts);
                    }
                    Endpoint::Rail(_) | Endpoint::BoardRail { .. } => {}
                }
            }
        }
    }
}

/// Build the netlist for the current board state.
///
/// `transient` decides only which elements count for *reachability*: a
/// capacitor is an open circuit at DC, so a net reached solely through one is
/// left floating in a DC solve, while a transient needs it as a node. Element
/// set and node numbering are otherwise identical, so a DC solve and the first
/// transient step describe the same circuit.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build(
    flat: &FlatBoard,
    drives: &BTreeMap<(u16, u8), Drive>,
    pwm: &BTreeMap<(u16, u8), f64>,
    parts: &[Box<dyn Part>],
    config: &AnalogConfig,
    transient: bool,
    limited: &BTreeSet<u16>,
    drooped: &BTreeMap<u16, f64>,
    converted: &BTreeMap<u16, f64>,
    powered: &[bool],
    driven: &BTreeMap<u16, f64>,
) -> Result<Circuit, String> {
    let net_count = flat.nets.len();
    let ground = net_count;
    let mut sets = DisjointSet::new(net_count + 1);

    // Rail nets are fixed nodes. `$gnd` is node 0; the other rails are ideal
    // sources to ground, so they are electrically tied to it for the purpose
    // of deciding which components are solvable.
    //
    // A board's own supply is a second table: its 3V3 is the regulator output
    // of that board, and its VSYS/VBUS pins are how an external source enters.
    // They are ideal sources too, at the rail's voltage when the board is
    // powered and at 0 V when it is not, so a circuit on a switched-off board
    // solves instead of floating.
    let mut rail_nets: BTreeMap<Rail, usize> = BTreeMap::new();
    let mut board_rail_nets: BTreeMap<(u16, Rail), usize> = BTreeMap::new();
    for (net, members) in flat.nets.iter().enumerate() {
        for endpoint in members {
            match *endpoint {
                Endpoint::Rail(rail) => {
                    rail_nets.entry(rail).or_insert(net);
                }
                Endpoint::BoardRail { board, rail } => {
                    board_rail_nets.entry((board, rail)).or_insert(net);
                }
                _ => {}
            }
        }
    }
    for net in rail_nets.values().chain(board_rail_nets.values()) {
        sets.union(*net, ground);
    }
    // A sensor with an unwired VCC pin falls back to 3.3 V. The global rail
    // wins when the diagram has one; otherwise the first board's regulator is
    // the supply it is meant.
    let v33_fallback = rail_nets
        .get(&Rail::V33)
        .copied()
        .or_else(|| board_rail_nets.get(&(0, Rail::V33)).copied());

    // Chip drivers are Thevenin sources to ground; a high-impedance pin adds
    // no path at all. `chip_drive` is keyed by `(board, GPIO)`, and a board
    // that is switched off has no driver at all. A pin a backend reports as a
    // running PWM holds the duty-weighted average of its two levels, which is
    // the voltage a meter or a load sees over a switching period.
    let board_on = |board: u16| powered.get(usize::from(board)).copied().unwrap_or(true);
    for (net, members) in flat.nets.iter().enumerate() {
        for endpoint in members {
            if let Some((board, gpio)) = endpoint.chip_pin()
                && board_on(board)
                && chip_volts(
                    drives.get(&(board, gpio)).copied().unwrap_or(Drive::HIGH_Z),
                    pwm.get(&(board, gpio)).copied(),
                    config,
                )
                .is_some()
            {
                sets.union(net, ground);
            }
        }
    }

    let mut models = Vec::new();
    for (index, part) in flat.parts.iter().enumerate() {
        if let Some(model) = part_model(
            flat,
            parts,
            index as u16,
            part,
            config,
            &Rails {
                global: &rail_nets,
                v33: v33_fallback,
            },
            limited,
            drooped,
            converted,
            driven,
        ) {
            model.union(&mut sets, transient);
            models.push(model);
        }
    }

    let ground_root = sets.find(ground);
    let mut solvable = |net: usize| sets.find(net) == ground_root;

    // Node 0 is ground. A net tied to ground by a rail gets node 0; any other
    // net in that component gets its own node. A component with no ground is
    // left out, so its nodes stay `None` (high impedance).
    let ground_net = rail_nets.get(&Rail::Gnd).copied();
    let mut node_of_net = vec![None; net_count];
    let mut next_node = 1usize;
    for (net, slot) in node_of_net.iter_mut().enumerate() {
        if !solvable(net) {
            continue;
        }
        if Some(net) == ground_net {
            *slot = Some(0);
        } else {
            *slot = Some(next_node);
            next_node += 1;
        }
    }

    // Count the internal nodes a model needs: one for an LED's series
    // resistor, one for a chip driver's Thevenin resistance.
    let mut internal = 0usize;
    for model in &models {
        internal += model.internal_nodes(&node_of_net);
    }
    for (net, members) in flat.nets.iter().enumerate() {
        if node_of_net[net].is_none() {
            continue;
        }
        for endpoint in members {
            if let Some((board, gpio)) = endpoint.chip_pin()
                && board_on(board)
                && chip_volts(
                    drives.get(&(board, gpio)).copied().unwrap_or(Drive::HIGH_Z),
                    pwm.get(&(board, gpio)).copied(),
                    config,
                )
                .is_some()
            {
                internal += 1;
            }
        }
    }

    let node_count = next_node + internal;
    let mut netlist = Netlist::new(node_count);
    let mut next_internal = next_node;
    let mut alloc = || {
        let node = next_internal;
        next_internal += 1;
        node
    };

    // Rails. `$gnd` is the reference and needs no element.
    for (&rail, &net) in &rail_nets {
        if rail == Rail::Gnd {
            continue;
        }
        let Some(node) = node_of_net[net] else {
            continue;
        };
        netlist
            .add(Element::VoltageSource {
                p: node,
                n: 0,
                volts: rail_volts(rail, config),
            })
            .map_err(|e| format!("analog: rail {rail:?}: {e}"))?;
    }
    // A board's supply: the rail's voltage while it is powered, zero volts
    // while it is not, so a switched-off board is a dead 0 V rail rather than
    // an unreferenced floating island.
    for (&(board, rail), &net) in &board_rail_nets {
        if rail == Rail::Gnd {
            continue;
        }
        let Some(node) = node_of_net[net] else {
            continue;
        };
        let volts = if powered.get(usize::from(board)).copied().unwrap_or(true) {
            rail_volts(rail, config)
        } else {
            0.0
        };
        netlist
            .add(Element::VoltageSource {
                p: node,
                n: 0,
                volts,
            })
            .map_err(|e| format!("analog: board {board} rail {rail:?}: {e}"))?;
    }

    // Driven chip pins: a voltage behind the driver's output resistance.
    for (net, members) in flat.nets.iter().enumerate() {
        let Some(node) = node_of_net[net] else {
            continue;
        };
        for endpoint in members {
            let Some((board, gpio)) = endpoint.chip_pin() else {
                continue;
            };
            if !board_on(board) {
                continue;
            }
            let Some(volts) = chip_volts(
                drives.get(&(board, gpio)).copied().unwrap_or(Drive::HIGH_Z),
                pwm.get(&(board, gpio)).copied(),
                config,
            ) else {
                continue;
            };
            let source = alloc();
            netlist
                .add(Element::VoltageSource {
                    p: source,
                    n: 0,
                    volts,
                })
                .map_err(|e| format!("analog: {} driver: {e}", chip_label(board, gpio)))?;
            netlist
                .add(Element::Resistor {
                    a: source,
                    b: node,
                    ohms: config.driver_ohms,
                })
                .map_err(|e| format!("analog: {} driver: {e}", chip_label(board, gpio)))?;
        }
    }

    let mut current_elements: Vec<(u16, ElementId)> = Vec::new();
    let mut leds = BTreeSet::new();
    for model in &models {
        model.build(
            &mut netlist,
            &node_of_net,
            &mut alloc,
            config,
            &mut current_elements,
            &mut leds,
        )?;
    }

    Ok(Circuit {
        netlist,
        node_of_net,
        nets: flat.nets.clone(),
        net_count,
        part_ids: flat.parts.iter().map(|part| part.id.clone()).collect(),
        pin_names: flat.parts.iter().map(|part| part.pins.clone()).collect(),
        current_elements,
        leds,
    })
}

/// The voltage a chip pin holds as a Thevenin source. High-Z and weak drives
/// return `None`: an input is not a source. A running PWM replaces the level
/// with its average, `duty × v33`, because a DC solve asks what the pin
/// delivers over time rather than which half of the cycle it is in.
fn chip_volts(drive: Drive, pwm_duty: Option<f64>, config: &AnalogConfig) -> Option<f64> {
    if let Some(duty) = pwm_duty {
        return Some(config.v33 * duty.clamp(0.0, 1.0));
    }
    if drive.strength != Strength::Strong {
        return None;
    }
    match drive.level {
        Level::High => Some(config.v33),
        Level::Low => Some(0.0),
        Level::HighZ => None,
    }
}

/// A chip pin as an error message names it. Board 0 keeps the plain GPIO the
/// single-board messages always used; another board is prefixed so a fault is
/// not attributed to the wrong chip.
fn chip_label(board: u16, gpio: u8) -> String {
    if board == 0 {
        format!("GPIO{gpio}")
    } else {
        format!("board {board} GPIO{gpio}")
    }
}

fn rail_volts(rail: Rail, config: &AnalogConfig) -> f64 {
    match rail {
        Rail::Gnd => 0.0,
        Rail::V33 => config.v33,
        Rail::V5 => config.v5,
        Rail::Vin => config.vin,
    }
}

/// One part's electrical model, with each terminal already resolved to a flat
/// net when its pin is wired.
enum Model {
    Resistor {
        part: u16,
        a: Option<usize>,
        b: Option<usize>,
        ohms: f64,
    },
    Led {
        part: u16,
        anode: Option<usize>,
        cathode: Option<usize>,
        color: String,
    },
    Potentiometer {
        end1: Option<usize>,
        wiper: Option<usize>,
        end3: Option<usize>,
        total_ohms: f64,
        travel: f64,
    },
    Diode {
        part: u16,
        anode: Option<usize>,
        cathode: Option<usize>,
        model: DiodeModel,
    },
    VoltageSource {
        part: u16,
        p: Option<usize>,
        n: Option<usize>,
        volts: f64,
    },
    /// A three-terminal linear regulator: `volts` at `out` referred to
    /// `ground`, until its input is closer than `dropout`, at which point it
    /// is the source in `drooped` (the input minus the dropout).
    ///
    /// An adjustable part whose `adjust` pin is wired regulates 1.25 V
    /// between `out` and that pin instead, so the resistor divider the
    /// firmware or the board put there sets the output. `volts` then only
    /// describes what the host dialed for an unwired part.
    Regulator {
        part: u16,
        out: Option<usize>,
        ground: Option<usize>,
        adjust: Option<usize>,
        volts: f64,
        drooped: Option<f64>,
    },
    /// A switching converter module: a source at `volts` from `out` to
    /// `ground`. A buck's setpoint is capped at its input, a boost's is
    /// floored at it.
    Converter {
        part: u16,
        out: Option<usize>,
        ground: Option<usize>,
        volts: f64,
    },
    CurrentSource {
        part: u16,
        p: Option<usize>,
        n: Option<usize>,
        amps: f64,
    },
    Capacitor {
        part: u16,
        a: Option<usize>,
        b: Option<usize>,
        farads: f64,
    },
    Inductor {
        part: u16,
        a: Option<usize>,
        b: Option<usize>,
        henries: f64,
    },
    /// A pushbutton: two contacts, each with two internally joined legs.
    Pushbutton {
        part: u16,
        /// `[[1.l, 1.r], [2.l, 2.r]]` resolved to nets.
        contacts: [[Option<usize>; 2]; 2],
        closed: bool,
    },
    /// An SPDT slide switch: common `2` rests on throw `1` or throw `3`.
    SlideSwitch {
        part: u16,
        throw1: Option<usize>,
        common: Option<usize>,
        throw3: Option<usize>,
        on: bool,
    },
    /// An NTC or photoresistor module's output, as a divider between a
    /// reference rail and ground at the part's control fraction.
    SensorDivider {
        ao: Option<usize>,
        reference: Option<usize>,
        ground: Option<usize>,
        fraction: f64,
        total_ohms: f64,
    },
    Bjt {
        part: u16,
        collector: Option<usize>,
        base: Option<usize>,
        emitter: Option<usize>,
        model: BjtModel,
    },
    Mosfet {
        part: u16,
        drain: Option<usize>,
        gate: Option<usize>,
        source: Option<usize>,
        model: MosfetModel,
    },
    OpAmp {
        plus: Option<usize>,
        minus: Option<usize>,
        out: Option<usize>,
        model: OpAmpModel,
    },
}

impl Model {
    /// Connectivity for deciding which components the solve can reference.
    fn union(&self, sets: &mut DisjointSet, transient: bool) {
        match self {
            Model::Resistor {
                a: Some(a),
                b: Some(b),
                ..
            } => sets.union(*a, *b),
            Model::Led {
                anode: Some(a),
                cathode: Some(c),
                ..
            }
            | Model::Diode {
                anode: Some(a),
                cathode: Some(c),
                ..
            } => sets.union(*a, *c),
            Model::Potentiometer {
                end1, wiper, end3, ..
            } => {
                if let (Some(a), Some(w)) = (end1, wiper) {
                    sets.union(*a, *w);
                }
                if let (Some(w), Some(b)) = (wiper, end3) {
                    sets.union(*w, *b);
                }
            }
            // An ideal voltage source ties its terminals together for this
            // purpose; a current source does not conduct and so does not.
            Model::VoltageSource {
                p: Some(p),
                n: Some(n),
                ..
            } => sets.union(*p, *n),
            // A regulator holds its output against its own ground pin, drooped
            // or not: the reference is the same terminal either way.
            Model::Regulator {
                out: Some(out),
                ground: Some(ground),
                ..
            } => sets.union(*out, *ground),
            Model::Converter {
                out: Some(out),
                ground: Some(ground),
                ..
            } => sets.union(*out, *ground),
            // A capacitor is a connection only when time is part of the
            // analysis; at DC it is an open circuit.
            Model::Capacitor {
                a: Some(a),
                b: Some(b),
                ..
            } if transient => sets.union(*a, *b),
            Model::Inductor {
                a: Some(a),
                b: Some(b),
                ..
            } => sets.union(*a, *b),
            Model::Pushbutton {
                contacts, closed, ..
            } => {
                for contact in contacts {
                    if let (Some(a), Some(b)) = (contact[0], contact[1]) {
                        sets.union(a, b);
                    }
                }
                if *closed
                    && let (Some(a), Some(b)) =
                        (first_wired(&contacts[0]), first_wired(&contacts[1]))
                {
                    sets.union(a, b);
                }
            }
            Model::SlideSwitch {
                throw1,
                common: Some(common),
                throw3,
                on,
                ..
            } => {
                let selected = if *on { *throw3 } else { *throw1 };
                if let Some(selected) = selected {
                    sets.union(*common, selected);
                }
            }
            Model::SensorDivider {
                ao: Some(ao),
                reference,
                ground,
                ..
            } => {
                if let Some(reference) = *reference {
                    sets.union(*ao, reference);
                }
                if let Some(ground) = *ground {
                    sets.union(*ao, ground);
                }
            }
            // A BJT conducts between all three terminals; an NMOS channel
            // conducts drain to source. An insulated gate and an op-amp input
            // are deliberately not tied to anything: a node with no DC path is
            // a wiring fault the solver reports rather than a silent short.
            Model::Bjt {
                collector: Some(c),
                base: Some(b),
                emitter: Some(e),
                ..
            } => {
                sets.union(*c, *e);
                sets.union(*b, *e);
            }
            Model::Mosfet {
                drain: Some(d),
                source: Some(s),
                ..
            } => sets.union(*d, *s),
            _ => {}
        }
    }

    fn internal_nodes(&self, node_of_net: &[Option<usize>]) -> usize {
        match self {
            Model::Led {
                anode: Some(a),
                cathode: Some(c),
                ..
            } => usize::from(
                node_of_net.get(*a).copied().flatten().is_some()
                    && node_of_net.get(*c).copied().flatten().is_some(),
            ),
            // The LM317's error amplifier needs one node the 1.25 V reference
            // source holds below its output.
            Model::Regulator {
                out: Some(out),
                adjust: Some(adjust),
                drooped: None,
                ..
            } => usize::from(
                node_of_net.get(*out).copied().flatten().is_some()
                    && node_of_net.get(*adjust).copied().flatten().is_some(),
            ),
            _ => 0,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        &self,
        netlist: &mut Netlist,
        node_of_net: &[Option<usize>],
        alloc: &mut impl FnMut() -> usize,
        config: &AnalogConfig,
        currents: &mut Vec<(u16, ElementId)>,
        leds: &mut BTreeSet<u16>,
    ) -> Result<(), String> {
        let node = |net: Option<usize>| -> Option<usize> {
            let net = net?;
            node_of_net.get(net).copied().flatten()
        };
        let add = |netlist: &mut Netlist, element: Element, what: &str| {
            netlist
                .add(element)
                .map_err(|e| format!("analog: {what}: {e}"))
        };

        match self {
            Model::Resistor { part, a, b, ohms } => {
                if let (Some(a), Some(b)) = (node(*a), node(*b)) {
                    let id = add(
                        netlist,
                        Element::Resistor {
                            a,
                            b,
                            ohms: (*ohms).max(MIN_OHMS),
                        },
                        "resistor",
                    )?;
                    currents.push((*part, id));
                }
            }
            Model::Led {
                part,
                anode,
                cathode,
                color,
            } => {
                if let (Some(anode), Some(cathode)) = (node(*anode), node(*cathode)) {
                    let junction = alloc();
                    let model = led_model(color);
                    let id = add(
                        netlist,
                        Element::Diode {
                            anode,
                            cathode: junction,
                            model,
                        },
                        "LED",
                    )?;
                    add(
                        netlist,
                        Element::Resistor {
                            a: junction,
                            b: cathode,
                            ohms: config.led_series_ohms.max(MIN_OHMS),
                        },
                        "LED series resistance",
                    )?;
                    currents.push((*part, id));
                    leds.insert(*part);
                }
            }
            Model::Potentiometer {
                end1,
                wiper,
                end3,
                total_ohms,
                travel,
            } => {
                let Some(wiper) = node(*wiper) else {
                    return Ok(());
                };
                // The track runs from 1 to 3. At travel 0 the wiper sits on
                // end 1, at 1 it sits on end 3.
                if let Some(end1) = node(*end1) {
                    add(
                        netlist,
                        Element::Resistor {
                            a: end1,
                            b: wiper,
                            ohms: (total_ohms * travel).max(MIN_OHMS),
                        },
                        "potentiometer track",
                    )?;
                }
                if let Some(end3) = node(*end3) {
                    add(
                        netlist,
                        Element::Resistor {
                            a: wiper,
                            b: end3,
                            ohms: (total_ohms * (1.0 - travel)).max(MIN_OHMS),
                        },
                        "potentiometer track",
                    )?;
                }
            }
            Model::Diode {
                part,
                anode,
                cathode,
                model,
            } => {
                if let (Some(anode), Some(cathode)) = (node(*anode), node(*cathode)) {
                    let id = add(
                        netlist,
                        Element::Diode {
                            anode,
                            cathode,
                            model: *model,
                        },
                        "diode",
                    )?;
                    currents.push((*part, id));
                }
            }
            Model::VoltageSource { part, p, n, volts } => {
                if let (Some(p), Some(n)) = (node(*p), node(*n)) {
                    let id = add(
                        netlist,
                        Element::VoltageSource {
                            p,
                            n,
                            volts: *volts,
                        },
                        "source",
                    )?;
                    currents.push((*part, id));
                }
            }
            Model::Regulator {
                part,
                out,
                ground,
                adjust,
                volts,
                drooped,
            } => {
                let Some(out) = node(*out) else {
                    return Ok(());
                };
                // An adjustable regulator with its adjust pin wired holds
                // 1.25 V between OUT and ADJ and drives OUT to whatever the
                // network on that pin asks for. That is an op-amp with the
                // reference offset: its non-inverting input is ADJ, its
                // inverting input is a node the 1.25 V source holds below
                // OUT. The reference node carries no other current, so the
                // divider's current flows through R1 and R2 as on the
                // datasheet instead of being shorted around by the source.
                if drooped.is_none()
                    && let Some(adjust) = (*adjust).and_then(|net| node(Some(net)))
                {
                    let reference = alloc();
                    let id = add(
                        netlist,
                        Element::VoltageSource {
                            p: out,
                            n: reference,
                            volts: LM317_REFERENCE_VOLTS,
                        },
                        "regulator reference",
                    )?;
                    currents.push((*part, id));
                    add(
                        netlist,
                        Element::OpAmp {
                            plus: adjust,
                            minus: reference,
                            out,
                            model: OpAmpModel {
                                gain: LM317_OPAMP_GAIN,
                                rail_low: 0.0,
                                rail_high: LM317_RAIL_VOLTS,
                            },
                        },
                        "regulator",
                    )?;
                    return Ok(());
                }
                let target = drooped.unwrap_or(*volts);
                let Some(ground) = node(*ground) else {
                    return Ok(());
                };
                let id = add(
                    netlist,
                    Element::VoltageSource {
                        p: out,
                        n: ground,
                        volts: target,
                    },
                    "regulator",
                )?;
                currents.push((*part, id));
            }
            Model::Converter {
                part,
                out,
                ground,
                volts,
            } => {
                if let (Some(out), Some(ground)) = (node(*out), node(*ground)) {
                    let id = add(
                        netlist,
                        Element::VoltageSource {
                            p: out,
                            n: ground,
                            volts: *volts,
                        },
                        "converter",
                    )?;
                    currents.push((*part, id));
                }
            }
            Model::CurrentSource { part, p, n, amps } => {
                if let (Some(p), Some(n)) = (node(*p), node(*n)) {
                    let id = add(
                        netlist,
                        Element::CurrentSource { p, n, amps: *amps },
                        "source",
                    )?;
                    // A source that folds back reads out like a supply: the
                    // caller sees the current it is actually delivering.
                    currents.push((*part, id));
                }
            }
            Model::Capacitor { part, a, b, farads } => {
                if let (Some(a), Some(b)) = (node(*a), node(*b)) {
                    let id = add(
                        netlist,
                        Element::Capacitor {
                            a,
                            b,
                            farads: (*farads).max(f64::MIN_POSITIVE),
                        },
                        "capacitor",
                    )?;
                    currents.push((*part, id));
                }
            }
            Model::Inductor {
                part,
                a,
                b,
                henries,
            } => {
                if let (Some(a), Some(b)) = (node(*a), node(*b)) {
                    let id = add(
                        netlist,
                        Element::Inductor {
                            a,
                            b,
                            henries: (*henries).max(f64::MIN_POSITIVE),
                        },
                        "inductor",
                    )?;
                    currents.push((*part, id));
                }
            }
            Model::Pushbutton {
                part,
                contacts,
                closed,
            } => {
                // Each contact is one conductor; legs are joined by a tiny
                // resistance so a wire to either leg reaches the switch.
                for contact in contacts {
                    if let (Some(a), Some(b)) = (node(contact[0]), node(contact[1])) {
                        add(
                            netlist,
                            Element::Resistor {
                                a,
                                b,
                                ohms: MIN_OHMS,
                            },
                            "pushbutton contact",
                        )?;
                    }
                }
                let a = node(first_wired(&contacts[0]));
                let b = node(first_wired(&contacts[1]));
                if let (Some(a), Some(b)) = (a, b) {
                    let id = add(
                        netlist,
                        Element::Resistor {
                            a,
                            b,
                            ohms: contact_ohms(*closed),
                        },
                        "pushbutton",
                    )?;
                    currents.push((*part, id));
                }
            }
            Model::SlideSwitch {
                part,
                throw1,
                common,
                throw3,
                on,
            } => {
                let Some(common) = node(*common) else {
                    return Ok(());
                };
                if let Some(throw1) = node(*throw1) {
                    let id = add(
                        netlist,
                        Element::Resistor {
                            a: common,
                            b: throw1,
                            ohms: contact_ohms(!*on),
                        },
                        "slide switch",
                    )?;
                    if !*on {
                        currents.push((*part, id));
                    }
                }
                if let Some(throw3) = node(*throw3) {
                    let id = add(
                        netlist,
                        Element::Resistor {
                            a: common,
                            b: throw3,
                            ohms: contact_ohms(*on),
                        },
                        "slide switch",
                    )?;
                    if *on {
                        currents.push((*part, id));
                    }
                }
            }
            Model::SensorDivider {
                ao,
                reference,
                ground,
                fraction,
                total_ohms,
            } => {
                let (Some(ao), Some(reference), Some(ground)) =
                    (node(*ao), node(*reference), node(*ground))
                else {
                    return Ok(());
                };
                // V(ao) = reference · fraction when nothing loads the divider.
                let fraction = fraction.clamp(0.0, 1.0);
                add(
                    netlist,
                    Element::Resistor {
                        a: reference,
                        b: ao,
                        ohms: (total_ohms * (1.0 - fraction)).max(MIN_OHMS),
                    },
                    "sensor divider",
                )?;
                add(
                    netlist,
                    Element::Resistor {
                        a: ao,
                        b: ground,
                        ohms: (total_ohms * fraction).max(MIN_OHMS),
                    },
                    "sensor divider",
                )?;
            }
            Model::Bjt {
                part,
                collector,
                base,
                emitter,
                model,
            } => {
                if let (Some(collector), Some(base), Some(emitter)) =
                    (node(*collector), node(*base), node(*emitter))
                {
                    let id = add(
                        netlist,
                        Element::Bjt {
                            collector,
                            base,
                            emitter,
                            model: *model,
                        },
                        "BJT",
                    )?;
                    currents.push((*part, id));
                }
            }
            Model::Mosfet {
                part,
                drain,
                gate,
                source,
                model,
            } => {
                if let (Some(drain), Some(gate), Some(source)) =
                    (node(*drain), node(*gate), node(*source))
                {
                    let id = add(
                        netlist,
                        Element::Mosfet {
                            drain,
                            gate,
                            source,
                            model: *model,
                        },
                        "MOSFET",
                    )?;
                    currents.push((*part, id));
                }
            }
            Model::OpAmp {
                plus,
                minus,
                out,
                model,
            } => {
                if let (Some(plus), Some(minus), Some(out)) =
                    (node(*plus), node(*minus), node(*out))
                {
                    add(
                        netlist,
                        Element::OpAmp {
                            plus,
                            minus,
                            out,
                            model: *model,
                        },
                        "op-amp",
                    )?;
                }
            }
        }
        Ok(())
    }
}

fn first_wired(legs: &[Option<usize>; 2]) -> Option<usize> {
    legs[0].or(legs[1])
}

fn contact_ohms(closed: bool) -> f64 {
    if closed {
        SWITCH_CLOSED_OHMS
    } else {
        SWITCH_OPEN_OHMS
    }
}

/// Read one part's electrical model from its engine and attributes.
/// The supply nets a part may be referred to when its own pins leave one
/// implicit: the diagram's shared rails and the 3.3 V net a sensor takes when
/// its VCC pin is unwired.
struct Rails<'a> {
    global: &'a BTreeMap<Rail, usize>,
    v33: Option<usize>,
}

#[allow(clippy::too_many_arguments)]
fn part_model(
    flat: &FlatBoard,
    parts: &[Box<dyn Part>],
    index: u16,
    part: &FlatPart,
    config: &AnalogConfig,
    rails: &Rails<'_>,
    limited: &BTreeSet<u16>,
    drooped: &BTreeMap<u16, f64>,
    converted: &BTreeMap<u16, f64>,
    driven: &BTreeMap<u16, f64>,
) -> Option<Model> {
    let Engine::Builtin(engine) = &part.engine else {
        return None;
    };
    let pin = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| part.pins.iter().position(|candidate| candidate == name))
            .map(|pin| pin as u16)
    };
    let net = |pin: Option<u16>| -> Option<usize> {
        let pin = pin?;
        flat.endpoint_net
            .get(&Endpoint::Part { part: index, pin })
            .map(|net| *net as usize)
    };
    let attr = |name: &str| part.attrs.get(name).map(String::as_str);
    let state = || parts.get(index as usize).map(|part| part.state());

    match engine.as_str() {
        "resistor" => {
            let ohms = attr("value").and_then(parse_resistance).unwrap_or(220.0);
            Some(Model::Resistor {
                part: index,
                a: net(pin(&["1"]).or(Some(0))),
                b: net(pin(&["2"]).or(Some(1))),
                ohms,
            })
        }
        // The meter is one two-terminal element whose resistance is the mode:
        // a voltmeter stands across the circuit, an ammeter in series with
        // it. Its reading is the voltage across (or current through) that same
        // element, so the host reads the solution rather than a part state.
        "multimeter" => {
            let ohms = match attr("mode") {
                Some("current") => AMMETER_OHMS,
                _ => VOLTMETER_OHMS,
            };
            Some(Model::Resistor {
                part: index,
                a: net(pin(&["+"])),
                b: net(pin(&["-"])),
                ohms,
            })
        }
        "led" => Some(Model::Led {
            part: index,
            anode: net(pin(&["A", "anode"])),
            cathode: net(pin(&["C", "cathode"])),
            color: attr("color").unwrap_or("red").to_string(),
        }),
        "potentiometer" => {
            let total_ohms = attr("resistance")
                .and_then(parse_resistance)
                .unwrap_or(config.pot_ohms);
            let travel = match state() {
                Some(PartState::Knob { permille }) => f64::from(permille) / 1000.0,
                _ => attr("value").map_or(0.5, parse_percent),
            };
            Some(Model::Potentiometer {
                end1: net(pin(&["1"])),
                wiper: net(pin(&["2", "W", "wiper"])),
                end3: net(pin(&["3"])),
                total_ohms,
                travel: travel.clamp(0.0, 1.0),
            })
        }
        "diode" => {
            // A zener is the same junction with a reverse breakdown: its `vz`
            // is the drop in reverse, so the forward half stays the plain
            // silicon knee rather than an LED curve.
            let breakdown = attr("vz")
                .or_else(|| attr("breakdown"))
                .or_else(|| attr("breakdown_voltage"))
                .and_then(parse_volts)
                .filter(|volts| *volts > 0.0);
            let vf = attr("vf")
                .or_else(|| attr("forward_voltage"))
                .or_else(|| attr("forward-voltage"))
                .and_then(parse_volts);
            let model = if breakdown.is_some() {
                // A zener's forward direction is a plain silicon junction, not
                // the softer curve an LED's ideality gives.
                diode_at(vf.unwrap_or(0.7), 1.0)
            } else {
                vf.map_or_else(DiodeModel::silicon, diode_at_forward_volts)
            };
            let model = breakdown.map_or(model, |vz| model.with_breakdown(vz));
            Some(Model::Diode {
                part: index,
                anode: net(pin(&["A", "anode"])),
                cathode: net(pin(&["C", "cathode"])),
                model,
            })
        }
        "voltage-regulator" => {
            // The regulator is a source referred to its ground pin. Whether it
            // can hold that setpoint is the fold in `build_operating`: one it
            // can hold is a source, one it cannot is the input minus the
            // dropout.
            let (setpoint, negative, _dropout) = regulator_spec(part, Some(index), parts)?;
            // Only the adjustable part has a feedback pin: a fixed regulator
            // with a wire on that name is a wiring mistake, not a divider.
            let adjustable = part
                .attrs
                .get("variant")
                .is_some_and(|variant| variant.trim() == "LM317");
            Some(Model::Regulator {
                part: index,
                out: net(pin(&["VO"])),
                ground: net(pin(&["GND"])),
                adjust: if adjustable { net(pin(&["ADJ"])) } else { None },
                volts: if negative { -setpoint } else { setpoint },
                drooped: drooped.get(&index).copied(),
            })
        }
        // A converter module is a source that its topology caps: the fold in
        // `build_operating` measures VI and hands in the output it can reach.
        "dc-dc" => {
            let out = net(pin(&["VO"]));
            let ground = net(pin(&["GND"]));
            if limited.contains(&index) {
                let amps = parts
                    .get(index as usize)
                    .and_then(|part| part.current_limit())
                    .map_or(0.0, |limit| -f64::from(limit));
                return Some(Model::CurrentSource {
                    part: index,
                    p: out,
                    n: ground,
                    amps,
                });
            }
            let setpoint = parts
                .get(index as usize)
                .and_then(|runtime| {
                    let pin = part.pins.iter().position(|name| name == "VO")? as u16;
                    runtime.analog(PinId(pin))
                })
                .map_or_else(
                    || {
                        attr("voltage")
                            .and_then(|text| text.trim().parse::<f64>().ok())
                            .unwrap_or(5.0)
                    },
                    f64::from,
                );
            Some(Model::Converter {
                part: index,
                out,
                ground,
                volts: converted.get(&index).copied().unwrap_or(setpoint),
            })
        }
        // A DAC on the bus is a source on its output pin. Its latched value is
        // read off the device by the runtime and handed in: the pin shell a
        // bus part gets has no state of its own.
        "mcp4725" | "pcf8591" => {
            let volts = driven.get(&index).copied().unwrap_or(0.0);
            let out = if engine == "pcf8591" { "AOUT" } else { "VOUT" };
            Some(Model::VoltageSource {
                part: index,
                p: net(pin(&[out])),
                n: net(pin(&["GND"])),
                volts,
            })
        }
        "voltage-source" | "dc-source" | "vsource" | "battery" => {
            let positive = pin(&["+", "V+", "P", "p", "1"]);
            let negative = pin(&["-", "V-", "N", "n", "2"]);
            // A bench supply reports its live setting; the attribute is the
            // starting point and a part without a live reading falls back to
            // it. A battery or a plain source has no control and never does.
            let volts = parts
                .get(index as usize)
                .and_then(|part| positive.and_then(|pin| part.analog(PinId(pin))))
                .map_or_else(
                    || {
                        attr("voltage")
                            .or_else(|| attr("volts"))
                            .or_else(|| attr("value"))
                            .and_then(parse_volts)
                            .unwrap_or(0.0)
                    },
                    f64::from,
                );
            if limited.contains(&index) {
                // Fold-back: hold the current, let the voltage land where the
                // load puts it. Current flows from the negative terminal
                // through the supply to the positive one, so the element's
                // `p`-to-`n` current is negative.
                let amps = parts
                    .get(index as usize)
                    .and_then(|part| part.current_limit())
                    .map_or(0.0, |limit| -f64::from(limit));
                return Some(Model::CurrentSource {
                    part: index,
                    p: net(positive),
                    n: net(negative),
                    amps,
                });
            }
            Some(Model::VoltageSource {
                part: index,
                p: net(positive),
                n: net(negative),
                volts,
            })
        }
        "signal-generator" => {
            // The engine owns the waveform; the source's value is whatever it
            // reports on OUT at the current phase. A pin that reports no
            // voltage contributes no source to the solve.
            let out_pin = pin(&["OUT"])?;
            let volts = parts.get(index as usize)?.analog(PinId(out_pin))?;
            Some(Model::VoltageSource {
                part: index,
                p: net(Some(out_pin)),
                n: net(pin(&["GND"])),
                volts: f64::from(volts),
            })
        }
        "current-source" | "isource" => {
            let amps = attr("amps")
                .or_else(|| attr("current"))
                .or_else(|| attr("value"))
                .and_then(parse_volts)
                .unwrap_or(0.0);
            Some(Model::CurrentSource {
                part: index,
                p: net(pin(&["+", "P", "p", "1"])),
                n: net(pin(&["-", "N", "n", "2"])),
                amps,
            })
        }
        "capacitor" => {
            let farads = attr("value")
                .or_else(|| attr("capacitance"))
                .and_then(parse_capacitance)
                .unwrap_or(1e-6);
            Some(Model::Capacitor {
                part: index,
                a: net(pin(&["1", "A", "+"])),
                b: net(pin(&["2", "B", "-"])),
                farads,
            })
        }
        "inductor" => {
            let henries = attr("value")
                .or_else(|| attr("inductance"))
                .and_then(parse_inductance)
                .unwrap_or(1e-3);
            Some(Model::Inductor {
                part: index,
                a: net(pin(&["1", "A"])),
                b: net(pin(&["2", "B"])),
                henries,
            })
        }
        "pushbutton" => {
            let legs = [
                [net(pin(&["1.l"])), net(pin(&["1.r"]))],
                [net(pin(&["2.l"])), net(pin(&["2.r"]))],
            ];
            let closed = matches!(state(), Some(PartState::Button { pressed: true }));
            Some(Model::Pushbutton {
                part: index,
                contacts: legs,
                closed,
            })
        }
        "slide-switch" => {
            let on = matches!(state(), Some(PartState::Switch { on: true }));
            Some(Model::SlideSwitch {
                part: index,
                throw1: net(pin(&["1"])),
                common: net(pin(&["2"])),
                throw3: net(pin(&["3"])),
                on,
            })
        }
        "ntc-temperature-sensor" => {
            let fraction = match state() {
                Some(PartState::Knob { permille }) => f64::from(permille) / 1000.0,
                _ => {
                    let celsius = attr("temperature")
                        .and_then(|text| text.trim().parse::<f64>().ok())
                        .unwrap_or(25.0);
                    ((celsius + 20.0) / 120.0).clamp(0.0, 1.0)
                }
            };
            Some(Model::SensorDivider {
                ao: net(pin(&["AO"])),
                reference: net(pin(&["VCC", "V+", "Vdd", "VDD"])).or(rails.v33),
                ground: net(pin(&["GND", "G"])).or_else(|| rail_net(rails.global, Rail::Gnd)),
                fraction,
                total_ohms: SENSOR_OHMS,
            })
        }
        "photoresistor-sensor" => {
            let fraction = match state() {
                Some(PartState::Knob { permille }) => f64::from(permille) / 1000.0,
                _ => attr("light")
                    .and_then(|text| text.trim().parse::<f64>().ok())
                    .map_or(0.5, |light| (light / 1000.0).clamp(0.0, 1.0)),
            };
            Some(Model::SensorDivider {
                ao: net(pin(&["AO"])),
                reference: net(pin(&["VCC", "V+", "Vdd", "VDD"])).or(rails.v33),
                ground: net(pin(&["GND", "G"])).or_else(|| rail_net(rails.global, Rail::Gnd)),
                fraction,
                total_ohms: SENSOR_OHMS,
            })
        }
        "bjt" => {
            // The solver's Ebers-Moll model is NPN. A PNP device is not the
            // mirror of these equations, so leave it unsolved rather than
            // compute a wrong operating point.
            if attr("polarity").is_some_and(|polarity| polarity.eq_ignore_ascii_case("pnp")) {
                return None;
            }
            let beta = attr("beta")
                .and_then(|text| text.trim().parse::<f64>().ok())
                .unwrap_or(100.0);
            let vbe_on = attr("vbe")
                .or_else(|| attr("vbe_on"))
                .and_then(parse_volts)
                .unwrap_or(0.7);
            let early_voltage = attr("early_voltage")
                .and_then(|text| text.trim().parse::<f64>().ok())
                .unwrap_or(100.0);
            Some(Model::Bjt {
                part: index,
                collector: net(pin(&["C", "collector", "c"])),
                base: net(pin(&["B", "base", "b"])),
                emitter: net(pin(&["E", "emitter", "e"])),
                model: BjtModel {
                    beta,
                    vbe_on,
                    early_voltage,
                },
            })
        }
        "mosfet" => {
            // The solver's level-1 model is NMOS; a P-channel device needs
            // sign-flipped equations the solver does not have.
            if attr("channel").is_some_and(|channel| channel.eq_ignore_ascii_case("p")) {
                return None;
            }
            let threshold = attr("vth")
                .or_else(|| attr("threshold"))
                .and_then(|text| text.trim().parse::<f64>().ok())
                .unwrap_or(2.0);
            let k = attr("k")
                .or_else(|| attr("beta"))
                .and_then(|text| text.trim().parse::<f64>().ok())
                .unwrap_or(1e-2);
            let lambda = attr("lambda")
                .and_then(|text| text.trim().parse::<f64>().ok())
                .unwrap_or(0.0);
            Some(Model::Mosfet {
                part: index,
                drain: net(pin(&["D", "drain", "d"])),
                gate: net(pin(&["G", "gate", "g"])),
                source: net(pin(&["S", "source", "s"])),
                model: MosfetModel {
                    threshold,
                    k,
                    lambda,
                },
            })
        }
        "opamp" | "op-amp" => {
            let gain = attr("gain")
                .and_then(|text| text.trim().parse::<f64>().ok())
                .unwrap_or(1e5);
            let rail_low = attr("rail_low")
                .and_then(|text| text.trim().parse::<f64>().ok())
                .unwrap_or(0.0);
            let rail_high = attr("rail_high")
                .and_then(|text| text.trim().parse::<f64>().ok())
                .unwrap_or(config.v33);
            Some(Model::OpAmp {
                plus: net(pin(&["+", "plus", "IN+", "INP", "P"])),
                minus: net(pin(&["-", "minus", "IN-", "INN", "N"])),
                out: net(pin(&["OUT", "out", "O"])),
                model: OpAmpModel {
                    gain,
                    rail_low,
                    rail_high,
                },
            })
        }
        _ => None,
    }
}

fn rail_net(rail_nets: &BTreeMap<Rail, usize>, rail: Rail) -> Option<usize> {
    rail_nets.get(&rail).copied()
}

/// An LED's forward voltage by colour, in volts. The values are the usual
/// through-hole part's nominal drop at its rated current.
fn led_forward_volts(color: &str) -> f64 {
    match color.to_ascii_lowercase().as_str() {
        "red" => 1.8,
        "green" => 2.0,
        "yellow" => 2.0,
        "orange" => 2.0,
        "blue" => 3.0,
        "white" => 3.2,
        "purple" | "pink" => 3.0,
        "cyan" => 3.0,
        _ => 1.8,
    }
}

/// A Shockley diode that drops `vf` at 20 mA, which is how an LED datasheet
/// states its forward voltage.
fn led_model(color: &str) -> DiodeModel {
    diode_at_forward_volts(led_forward_volts(color))
}

fn diode_at_forward_volts(vf: f64) -> DiodeModel {
    diode_at(vf, 2.0)
}

/// A Shockley diode that drops `vf` at 20 mA with the given ideality. An LED
/// is fitted with `n = 2`; a zener's forward knee is a silicon junction, `n = 1`.
fn diode_at(vf: f64, ideality: f64) -> DiodeModel {
    const NOMINAL_AMPS: f64 = 0.02;
    let scale = (vf / (ideality * THERMAL_VOLTAGE)).exp() - 1.0;
    let saturation_current = if scale > 0.0 {
        NOMINAL_AMPS / scale
    } else {
        1e-14
    };
    DiodeModel::led(saturation_current, ideality)
}

/// Parse a resistance as a diagram writes it: `220`, `10k`, `4k7`, `1M`,
/// `2.2k`, `4R7`. Returns `None` for text that is not a number.
pub(crate) fn parse_resistance(text: &str) -> Option<f64> {
    let cleaned = text.trim().replace([' ', '_'], "");
    let mut text = cleaned.as_str();
    for suffix in ["ohms", "ohm", "Ohms", "Ohm", "OHM", "OHMS", "Ω", "ω"] {
        if let Some(stripped) = text.strip_suffix(suffix) {
            text = stripped.trim();
            break;
        }
    }
    if text.is_empty() {
        return None;
    }
    // Plain decimal or scientific notation, including "220" and "1e3".
    if let Ok(value) = text.parse::<f64>() {
        return (value > 0.0).then_some(value);
    }
    let split = text.find(|c: char| c.is_ascii_alphabetic())?;
    let (leading, rest) = text.split_at(split);
    let mut chars = rest.chars();
    let prefix = chars.next()?;
    let trailing = chars.as_str();
    let factor = match prefix {
        'k' | 'K' => 1e3,
        'M' => 1e6,
        'm' => 1e-3,
        'G' | 'g' => 1e9,
        'R' | 'r' => 1.0,
        _ => return None,
    };
    let value: f64 = if trailing.is_empty() {
        leading.parse().ok()?
    } else {
        format!("{leading}.{trailing}").parse().ok()?
    };
    let ohms = value * factor;
    (ohms > 0.0).then_some(ohms)
}

/// Parse a capacitance as a diagram writes it: `100n`, `100nF`, `4.7u`,
/// `2.2µF`, `10p`, `1mF`. A suffix without a unit letter is the common
/// shorthand and means farads. Returns `None` for text that is not a number.
pub(crate) fn parse_capacitance(text: &str) -> Option<f64> {
    parse_si_value(text, &["F", "f"])
}

/// Parse an inductance as a diagram writes it: `10u`, `10uH`, `4.7mH`,
/// `100nH`. As with [`parse_capacitance`], the trailing `H` is optional.
pub(crate) fn parse_inductance(text: &str) -> Option<f64> {
    parse_si_value(text, &["H", "h"])
}

/// Parse a value with an SI prefix and an optional unit letter.
fn parse_si_value(text: &str, units: &[&str]) -> Option<f64> {
    // Normalise the micro sign freely: diagrams use `u` and the two Unicode
    // spellings interchangeably.
    let cleaned = text.trim().replace([' ', '_'], "").replace(['µ', 'μ'], "u");
    let mut text = cleaned.as_str();
    for unit in units {
        if let Some(stripped) = text.strip_suffix(unit) {
            text = stripped;
            break;
        }
    }
    if text.is_empty() {
        return None;
    }
    if let Ok(value) = text.parse::<f64>() {
        return (value > 0.0).then_some(value);
    }
    let split = text.find(|c: char| c.is_ascii_alphabetic())?;
    let (leading, rest) = text.split_at(split);
    let mut chars = rest.chars();
    let prefix = chars.next()?;
    let trailing = chars.as_str();
    let factor = match prefix {
        'p' | 'P' => 1e-12,
        'n' | 'N' => 1e-9,
        'u' | 'U' => 1e-6,
        'm' => 1e-3,
        'k' | 'K' => 1e3,
        'M' => 1e6,
        'G' | 'g' => 1e9,
        _ => return None,
    };
    let value: f64 = if trailing.is_empty() {
        leading.parse().ok()?
    } else {
        format!("{leading}.{trailing}").parse().ok()?
    };
    let value = value * factor;
    (value > 0.0).then_some(value)
}

/// Parse a voltage as a diagram writes it: `5`, `3.3`, `5V`.
fn parse_volts(text: &str) -> Option<f64> {
    let text = text.trim().trim_end_matches(['V', 'v']);
    text.trim().parse().ok()
}

/// The catalog writes a potentiometer's initial position as 0–100 percent.
fn parse_percent(text: &str) -> f64 {
    text.trim()
        .parse::<f64>()
        .map_or(0.5, |percent| (percent / 100.0).clamp(0.0, 1.0))
}

/// Union-find over net indices plus one sentinel for ground.
struct DisjointSet {
    parent: Vec<usize>,
}

impl DisjointSet {
    fn new(len: usize) -> Self {
        Self {
            parent: (0..len).collect(),
        }
    }

    fn find(&mut self, mut index: usize) -> usize {
        while self.parent[index] != index {
            self.parent[index] = self.parent[self.parent[index]];
            index = self.parent[index];
        }
        index
    }

    fn union(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.parent[a.max(b)] = a.min(b);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resistance_text_uses_electronics_notation() {
        assert_eq!(parse_resistance("220"), Some(220.0));
        assert_eq!(parse_resistance("10k"), Some(10_000.0));
        assert_eq!(parse_resistance("4k7"), Some(4_700.0));
        assert_eq!(parse_resistance("4K7"), Some(4_700.0));
        assert_eq!(parse_resistance("2.2k"), Some(2_200.0));
        assert_eq!(parse_resistance("1M"), Some(1_000_000.0));
        assert_eq!(parse_resistance("4R7"), Some(4.7));
        assert_eq!(parse_resistance("1e3"), Some(1_000.0));
        assert_eq!(parse_resistance("10 k"), Some(10_000.0));
        assert_eq!(parse_resistance("4k7Ω"), Some(4_700.0));
        assert_eq!(parse_resistance("nonsense"), None);
        assert_eq!(parse_resistance("0"), None);
    }

    #[test]
    fn capacitance_and_inductance_text_accept_si_prefixes() {
        // The prefix multiplication is not exact in binary, so compare to a
        // relative tolerance rather than bit-for-bit.
        let close = |actual: Option<f64>, expected: f64| {
            let actual = actual.expect("the text parses");
            assert!(
                (actual - expected).abs() <= expected.abs() * 1e-12,
                "{actual:e} F/H is not {expected:e}"
            );
        };
        close(parse_capacitance("100n"), 100e-9);
        close(parse_capacitance("100nF"), 100e-9);
        close(parse_capacitance("4.7u"), 4.7e-6);
        close(parse_capacitance("2.2µF"), 2.2e-6);
        close(parse_capacitance("10p"), 10e-12);
        close(parse_capacitance("1mF"), 1e-3);
        assert_eq!(parse_capacitance("nonsense"), None);
        close(parse_inductance("10uH"), 10e-6);
        close(parse_inductance("4.7mH"), 4.7e-3);
        close(parse_inductance("100n"), 100e-9);
    }

    #[test]
    fn an_led_model_drops_its_forward_voltage_at_the_rated_current() {
        let model = led_model("green");
        let vf = 2.0 * THERMAL_VOLTAGE * (0.02 / model.saturation_current + 1.0).ln();
        assert!((vf - 2.0).abs() < 1e-9, "{vf}");
    }
}
