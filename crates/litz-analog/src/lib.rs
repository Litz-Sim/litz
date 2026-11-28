//! A deterministic modified-nodal-analysis solver for small analog circuits.
//!
//! This crate is the analog engine behind Litz's optional SPICE-like analysis.
//! It is deliberately self-contained: pure Rust, no C libraries, no linear
//! algebra dependency. The solver is *modified nodal analysis* (MNA): node
//! voltages are unknowns, and ideal voltage sources (plus inductors at DC, and
//! op-amp outputs) add one branch current each. Nonlinear devices — diodes,
//! BJTs, MOSFETs and op-amps — are linearised at each Newton-Raphson iteration
//! and solved by repeated dense LU factorisation with partial pivoting.
//!
//! A small-signal AC sweep ([`Netlist::ac_sweep`]) reuses the same netlist: it
//! solves the DC operating point, linearises every nonlinear device there, and
//! solves the complex nodal system at each requested frequency. See
//! [`AcRequest`] for the frequency forms and [`AcSweep`] for the result.
//!
//! The engine is meant for teaching-scale circuits and for checkable operating
//! points, not for RF, parasitics or large netlists. It is built to be
//! predictable: a given netlist produces the same bits on every host, because
//! the solver is a fixed sequence of floating-point operations with no
//! threading, randomness or sparse reordering.
//!
//! # Units
//!
//! Everything is SI, with no prefixes at the API boundary:
//!
//! * node voltages: volts (V)
//! * currents: amperes (A)
//! * resistance: ohms (Ω)
//! * capacitance: farads (F)
//! * inductance: henries (H)
//! * time: seconds (s)
//! * frequency: hertz (Hz); phase in degrees
//!
//! So a 4.7 kΩ resistor is `4_700.0`, a 100 nF capacitor is `100e-9`.
//!
//! # Nodes and ground
//!
//! Node `0` is ground and is never an unknown. Every other node is numbered
//! `1..node_count`; create the netlist with its total node count *including*
//! ground:
//!
//! ```
//! use litz_analog::{Element, Netlist};
//!
//! // A 10 V source across a 1 kΩ + 1 kΩ divider.
//! let mut net = Netlist::new(3); // ground, mid, top
//! net.add(Element::VoltageSource { p: 2, n: 0, volts: 10.0 }).unwrap();
//! net.add(Element::Resistor { a: 2, b: 1, ohms: 1_000.0 }).unwrap();
//! net.add(Element::Resistor { a: 1, b: 0, ohms: 1_000.0 }).unwrap();
//! let dc = net.solve_dc().unwrap();
//! assert!((dc.node_volts[1] - 5.0).abs() < 1e-9);
//! ```
//!
//! # Sign convention
//!
//! Two-terminal elements carry an explicit direction `a -> b` (or `p -> n`).
//! Their reported current is positive when it flows from the first terminal to
//! the second *through the element*:
//!
//! * resistor: `(V[a] - V[b]) / R`
//! * voltage source / inductor: the branch current from `p` to `n` through the
//!   source itself (an ideal source driving a load reports a negative value)
//! * current source: its `amps`, defined from `p` to `n` through the source
//! * capacitor: `C * d(V[a] - V[b]) / dt`
//!
//! Three-terminal devices report their main channel: collector current for a
//! BJT, drain current for a MOSFET, output current for an op-amp.
//!
//! # Determinism
//!
//! Nothing in the solve depends on hash iteration order, wall-clock time or
//! address values. Circuit elements are assembled in insertion order, pivoting
//! breaks ties by keeping the earlier row, and the Newton loop has a fixed
//! iteration cap. Solving the same netlist twice in a given build yields
//! bit-identical [`f64`]s; the `deterministic_same_netlist_twice` test asserts
//! this.
//!
//! # Limits
//!
//! * Dense LU: `O(N^3)` time and `O(N^2)` memory for `N` unknowns. Fine for
//!   tens of nodes, not thousands.
//! * Constant temperature `300 K` ([`THERMAL_VOLTAGE`]); no self-heating.
//! * Diode: Shockley equation only (no series resistance or junction
//!   capacitance). BJT: NPN Ebers-Moll transport model with forward/reverse
//!   beta and an optional Early voltage. MOSFET: NMOS level-1 square law.
//! * Op-amp: finite gain with hard output rails, zero input current, zero
//!   output impedance; not a small-signal AC model.
//! * Transient: backward Euler only (first order), fixed or caller-chosen
//!   step, no automatic step control.
//! * AC: small-signal about the DC point only. Device models carry no
//!   frequency dependence (no junction capacitance, transistor charge storage
//!   or op-amp gain-bandwidth), and a plain [`Element::VoltageSource`] is a
//!   zero AC source. Use [`Element::AcVoltageSource`] to drive the sweep.
//! * Ideal voltage sources and inductors at DC; a loop of ideal voltage
//!   sources or a floating node is reported as [`Error::Singular`].
#![forbid(unsafe_code)]

mod ac;
mod complex;
mod dc;
mod device;
mod linalg;
mod mna;
mod transient;

pub use ac::{AcPoint, AcRequest, AcSweep};
pub use complex::Complex;
pub use dc::solve_dc;
pub use linalg::{LinalgError, PIVOT_EPSILON};
pub use transient::{Sample, Transient};

use thiserror::Error;

/// Node number. Node `0` is ground.
pub type Node = usize;

/// Ground node number.
pub const GROUND: Node = 0;

/// Largest node count the dense solver will accept.
///
/// The matrix is `N × N`; 1024 nodes plus branches is already a few megabytes
/// per factorisation. Anything larger belongs in a sparse solver, which this
/// crate deliberately is not.
pub const MAX_NODES: usize = 1024;

/// Thermal voltage `k·T/q` at `300 K`, in volts.
///
/// The junction equations use this as their scale factor; a diode's emission
/// coefficient divides it further.
pub const THERMAL_VOLTAGE: f64 = 0.025_852;

/// Identifies an element in a [`Netlist`].
///
/// The value is the element's insertion index. It stays valid as long as the
/// netlist is not rebuilt; it is not a stable serialisation id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ElementId(pub usize);

/// Why a netlist could not be solved.
#[derive(Debug, Error)]
pub enum Error {
    /// A netlist needs at least one node, and node `0` must exist.
    #[error("netlist has no ground node (node_count must be at least 1)")]
    NoGround,

    /// An element refers to a node outside `0..node_count`.
    #[error(
        "element refers to node {node}, but the netlist has {node_count} nodes (0..{node_count})"
    )]
    InvalidNode { node: Node, node_count: usize },

    /// A model parameter is zero, negative, NaN or infinite where a positive
    /// finite value is required.
    #[error("element {element} has an invalid parameter: {reason}")]
    InvalidParameter {
        element: usize,
        reason: &'static str,
    },

    /// A caller asked for an element that is not in the netlist.
    #[error("element {0:?} is not in this netlist")]
    UnknownElement(ElementId),

    /// The MNA matrix is singular: a floating node, a voltage-source loop, or a
    /// current source with nowhere to push its current.
    #[error(transparent)]
    Singular(#[from] linalg::LinalgError),

    /// Newton-Raphson did not settle within its iteration budget.
    #[error(
        "Newton-Raphson did not converge after {iterations} iterations (last node delta {residual:e} V)"
    )]
    NotConverged { iterations: usize, residual: f64 },

    /// A transient step needs a positive, finite time step.
    #[error("invalid time step {0}: must be positive and finite")]
    InvalidTimeStep(f64),

    /// A transient run needs a positive, finite duration.
    #[error("invalid transient duration {0}: must be positive and finite")]
    InvalidDuration(f64),

    /// A transient run would take an unreasonable number of steps.
    #[error("transient run needs {steps} steps, above the cap")]
    TooManySteps { steps: f64 },

    /// The netlist has more nodes than the dense solver accepts.
    #[error("netlist has {nodes} nodes; the dense solver accepts at most {MAX_NODES}")]
    TooLarge { nodes: usize },

    /// A current was requested before the first transient step was solved.
    #[error("transient has not been stepped yet")]
    NoSolutionYet,

    /// An AC sweep request was malformed: a non-positive or non-finite
    /// frequency, an empty explicit list, or a zero `points_per_decade`.
    #[error("invalid AC sweep: {0}")]
    InvalidAcRequest(&'static str),

    /// An AC sweep request would evaluate an unreasonable number of points.
    #[error("AC sweep asks for {points} points, above the cap")]
    TooManyAcPoints { points: usize },
}

/// A diode model.
///
/// The forward junction follows the Shockley equation
/// `I = Is·(exp(V/(n·Vt)) - 1)` with `Vt` = [`THERMAL_VOLTAGE`]. At `300 K` a
/// silicon diode with `Is = 1e-14 A`, `n = 1` drops about 0.65 V at 1 mA; a
/// visible LED is better modelled with a larger `n` and a much smaller `Is`
/// (for example `Is = 1e-17`, `n = 2`, about 1.8 V at 20 mA).
///
/// [`DiodeModel::breakdown_volts`] adds a second, reverse junction so the same
/// element can be a zener: once the cathode sits `Vz` above the anode, the
/// diode conducts backwards with the same exponential knee instead of blocking
/// forever. Without it the reverse current is the forward saturation current,
/// which is what an ordinary rectifier does until it avalanches at a voltage
/// far outside a board run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DiodeModel {
    /// Saturation current `Is`, in amperes. Must be positive.
    pub saturation_current: f64,
    /// Emission coefficient `n` (ideality factor). Must be positive; `1.0` is
    /// ideal silicon, `~2.0` fits an LED.
    pub emission_coefficient: f64,
    /// Reverse breakdown voltage `Vz`, in volts, as a positive magnitude, at
    /// a test current of 5 mA — the way a zener datasheet quotes it. The diode
    /// conducts in reverse at this drop; `None` means no breakdown.
    pub breakdown_volts: Option<f64>,
}

impl DiodeModel {
    /// A small-signal silicon diode: `Is = 1e-14 A`, `n = 1`, no breakdown.
    pub const fn silicon() -> Self {
        Self {
            saturation_current: 1e-14,
            emission_coefficient: 1.0,
            breakdown_volts: None,
        }
    }

    /// An LED with the given ideality and saturation current.
    pub const fn led(saturation_current: f64, emission_coefficient: f64) -> Self {
        Self {
            saturation_current,
            emission_coefficient,
            breakdown_volts: None,
        }
    }

    /// A zener with the given breakdown voltage and `Is = 1e-14 A`, `n = 1`.
    /// The reverse drop is `Vz` at a 5 mA test current.
    pub const fn zener(breakdown_volts: f64) -> Self {
        Self {
            saturation_current: 1e-14,
            emission_coefficient: 1.0,
            breakdown_volts: Some(breakdown_volts),
        }
    }

    /// The same diode with a reverse breakdown voltage `Vz`, in volts.
    pub const fn with_breakdown(self, breakdown_volts: f64) -> Self {
        Self {
            breakdown_volts: Some(breakdown_volts),
            ..self
        }
    }

    fn validate(&self) -> Result<(), &'static str> {
        if !positive_finite(self.saturation_current) {
            return Err("diode saturation_current must be positive and finite");
        }
        if !positive_finite(self.emission_coefficient) {
            return Err("diode emission_coefficient must be positive and finite");
        }
        if let Some(breakdown) = self.breakdown_volts
            && !positive_finite(breakdown)
        {
            return Err("diode breakdown_volts must be positive and finite when given");
        }
        Ok(())
    }
}

/// An NPN bipolar transistor model.
///
/// The base-emitter and base-collector junctions are diode junctions evaluated
/// at [`THERMAL_VOLTAGE`]; `saturation_current` is the transport saturation
/// current. The model is the Ebers-Moll transport form with forward beta
/// `beta`, reverse beta `1.0`, and an Early voltage that scales the transport
/// current by `1 + Vce/early_voltage`. The Early effect is never fully
/// disabled: validation requires a positive finite voltage, so use a very
/// large value such as `1e12` when it should not matter.
///
/// The `vbe_on` field is the forward base-emitter drop at 1 mA of collector
/// current, which is how a datasheet states the turn-on voltage. The model
/// derives its transport saturation current from it, so `vbe_on = 0.7 V` gives
/// a transistor that carries about 1 mA at `Vbe ≈ 0.7 V`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BjtModel {
    /// Forward current gain `beta` (`Ic / Ib` in the active region). Positive.
    pub beta: f64,
    /// Base-emitter voltage at 1 mA collector current, in volts. Positive.
    pub vbe_on: f64,
    /// Early voltage in volts. Larger values mean flatter output curves;
    /// `f64::INFINITY` is not allowed (use a large finite value).
    pub early_voltage: f64,
}

impl BjtModel {
    /// A typical small-signal NPN: `beta = 100`, `Vbe(on) = 0.7 V`, Early
    /// voltage `100 V`.
    pub const fn npn() -> Self {
        Self {
            beta: 100.0,
            vbe_on: 0.7,
            early_voltage: 100.0,
        }
    }

    /// An NPN with an explicit current gain and turn-on voltage.
    pub const fn with_beta(beta: f64, vbe_on: f64) -> Self {
        Self {
            beta,
            vbe_on,
            early_voltage: 100.0,
        }
    }

    /// Transport saturation current implied by `vbe_on` at 1 mA.
    fn saturation_current(&self) -> f64 {
        let vt = THERMAL_VOLTAGE;
        1e-3 * (-self.vbe_on / vt).exp()
    }

    fn validate(&self) -> Result<(), &'static str> {
        if !positive_finite(self.beta) {
            return Err("BJT beta must be positive and finite");
        }
        if !positive_finite(self.vbe_on) {
            return Err("BJT vbe_on must be positive and finite");
        }
        if !positive_finite(self.early_voltage) {
            return Err("BJT early_voltage must be positive and finite");
        }
        if !positive_finite(self.saturation_current()) {
            return Err("BJT vbe_on does not produce a usable saturation current");
        }
        Ok(())
    }
}

/// An enhancement-mode NMOS transistor model (level 1, square law).
///
/// In the triode region (`Vds < Vgs - Vth`) the drain current is
/// `K·((Vgs - Vth)·Vds - Vds²/2)`; in saturation it is
/// `K/2·(Vgs - Vth)²·(1 + lambda·Vds)`. `K` is the transconductance parameter
/// `µ·Cox·W/L`, in A/V². The body is tied to the source; PMOS is not modelled.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MosfetModel {
    /// Threshold voltage `Vth`, in volts. May be zero or positive.
    pub threshold: f64,
    /// Transconductance parameter `K`, in A/V². Must be positive.
    pub k: f64,
    /// Channel-length modulation `lambda`, in 1/V. Zero is ideal.
    pub lambda: f64,
}

impl MosfetModel {
    /// An ideal NMOS with the given threshold and `K`.
    pub const fn nmos(threshold: f64, k: f64) -> Self {
        Self {
            threshold,
            k,
            lambda: 0.0,
        }
    }

    fn validate(&self) -> Result<(), &'static str> {
        if !self.threshold.is_finite() {
            return Err("MOSFET threshold must be finite");
        }
        if !positive_finite(self.k) {
            return Err("MOSFET k must be positive and finite");
        }
        if !self.lambda.is_finite() || self.lambda < 0.0 {
            return Err("MOSFET lambda must be finite and non-negative");
        }
        Ok(())
    }
}

/// A voltage-mode op-amp model.
///
/// The output follows `clamp(gain·(V+ - V-), rail_low, rail_high)`. Input
/// current is zero and output impedance is zero. `gain` may be finite and
/// positive (for example `1e5`); ideal infinite gain is not modelled because
/// the hard clamp already supplies the rails.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OpAmpModel {
    /// Open-loop gain, V/V. Positive.
    pub gain: f64,
    /// Lower output rail in volts (usually the negative supply).
    pub rail_low: f64,
    /// Upper output rail in volts (usually the positive supply).
    pub rail_high: f64,
}

impl OpAmpModel {
    /// A rail-to-rail 5 V op-amp with gain `1e5`.
    pub const fn rail_to_rail_5v() -> Self {
        Self {
            gain: 1e5,
            rail_low: 0.0,
            rail_high: 5.0,
        }
    }

    fn validate(&self) -> Result<(), &'static str> {
        if !positive_finite(self.gain) {
            return Err("op-amp gain must be positive and finite");
        }
        if !self.rail_low.is_finite() || !self.rail_high.is_finite() {
            return Err("op-amp rails must be finite");
        }
        if self.rail_low >= self.rail_high {
            return Err("op-amp rail_low must be below rail_high");
        }
        Ok(())
    }
}

/// One two- or multi-terminal element in a [`Netlist`].
///
/// See the module documentation for the current sign convention on each
/// terminal pair.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Element {
    /// A linear resistor between `a` and `b`, in ohms.
    Resistor { a: Node, b: Node, ohms: f64 },
    /// An ideal voltage source holding `V[p] - V[n] = volts`.
    VoltageSource { p: Node, n: Node, volts: f64 },
    /// An ideal AC voltage source: a phasor `amplitude∠phase_deg` at every
    /// swept frequency, and exactly `0 V` at DC and in transient.
    ///
    /// Use this to drive a filter in [`Netlist::ac_sweep`]. A plain
    /// [`Element::VoltageSource`] is a *zero* AC source (an AC ground) in the
    /// same sweep, so a circuit's DC bias and its AC stimulus are independent.
    AcVoltageSource {
        p: Node,
        n: Node,
        /// Peak amplitude in volts. Must be finite and non-negative.
        amplitude: f64,
        /// Phase in degrees. Must be finite.
        phase_deg: f64,
    },
    /// An ideal current source forcing `amps` from `p` to `n`.
    CurrentSource { p: Node, n: Node, amps: f64 },
    /// A linear capacitor between `a` and `b`, in farads. Open at DC.
    Capacitor { a: Node, b: Node, farads: f64 },
    /// A linear inductor between `a` and `b`, in henries. A short at DC.
    Inductor { a: Node, b: Node, henries: f64 },
    /// A diode with its anode at `anode` and cathode at `cathode`.
    Diode {
        anode: Node,
        cathode: Node,
        model: DiodeModel,
    },
    /// An NPN bipolar transistor.
    Bjt {
        collector: Node,
        base: Node,
        emitter: Node,
        model: BjtModel,
    },
    /// An enhancement-mode NMOS transistor, body tied to source.
    Mosfet {
        drain: Node,
        gate: Node,
        source: Node,
        model: MosfetModel,
    },
    /// A voltage-mode op-amp with `out = clamp(gain·(plus - minus), rails)`.
    OpAmp {
        plus: Node,
        minus: Node,
        out: Node,
        model: OpAmpModel,
    },
}

impl Element {
    fn nodes(&self) -> ([Node; 3], usize) {
        match *self {
            Self::Resistor { a, b, .. }
            | Self::Capacitor { a, b, .. }
            | Self::Inductor { a, b, .. } => ([a, b, 0], 2),
            Self::VoltageSource { p, n, .. }
            | Self::AcVoltageSource { p, n, .. }
            | Self::CurrentSource { p, n, .. } => ([p, n, 0], 2),
            Self::Diode { anode, cathode, .. } => ([anode, cathode, 0], 2),
            Self::Bjt {
                collector,
                base,
                emitter,
                ..
            } => ([collector, base, emitter], 3),
            Self::Mosfet {
                drain,
                gate,
                source,
                ..
            } => ([drain, gate, source], 3),
            Self::OpAmp {
                plus, minus, out, ..
            } => ([plus, minus, out], 3),
        }
    }

    fn validate(&self, index: usize, node_count: usize) -> Result<(), Error> {
        let (nodes, count) = self.nodes();
        for &node in &nodes[..count] {
            if node >= node_count {
                return Err(Error::InvalidNode { node, node_count });
            }
        }
        let bad = |reason| Error::InvalidParameter {
            element: index,
            reason,
        };
        match *self {
            Self::Resistor { ohms, .. } => {
                if !positive_finite(ohms) {
                    return Err(bad("resistance must be positive and finite"));
                }
            }
            Self::VoltageSource { volts, .. } => {
                if !volts.is_finite() {
                    return Err(bad("source voltage must be finite"));
                }
            }
            Self::AcVoltageSource {
                amplitude,
                phase_deg,
                ..
            } => {
                if !amplitude.is_finite() || amplitude < 0.0 {
                    return Err(bad("AC source amplitude must be finite and non-negative"));
                }
                if !phase_deg.is_finite() {
                    return Err(bad("AC source phase must be finite degrees"));
                }
            }
            Self::CurrentSource { amps, .. } => {
                if !amps.is_finite() {
                    return Err(bad("source current must be finite"));
                }
            }
            Self::Capacitor { farads, .. } => {
                if !positive_finite(farads) {
                    return Err(bad("capacitance must be positive and finite"));
                }
            }
            Self::Inductor { henries, .. } => {
                if !positive_finite(henries) {
                    return Err(bad("inductance must be positive and finite"));
                }
            }
            Self::Diode { model, .. } => model.validate().map_err(bad)?,
            Self::Bjt { model, .. } => model.validate().map_err(bad)?,
            Self::Mosfet { model, .. } => model.validate().map_err(bad)?,
            Self::OpAmp { model, .. } => model.validate().map_err(bad)?,
        }
        Ok(())
    }

    /// Whether the element needs a branch-current unknown at DC.
    fn has_dc_branch(&self) -> bool {
        matches!(
            self,
            Self::VoltageSource { .. }
                | Self::AcVoltageSource { .. }
                | Self::Inductor { .. }
                | Self::OpAmp { .. }
        )
    }

    /// Whether the element needs a branch-current unknown in transient.
    fn has_transient_branch(&self) -> bool {
        matches!(
            self,
            Self::VoltageSource { .. } | Self::AcVoltageSource { .. } | Self::OpAmp { .. }
        )
    }
}

fn positive_finite(x: f64) -> bool {
    x.is_finite() && x > 0.0
}

/// A circuit: a node count and an ordered list of elements.
///
/// Construction is fallible per element: [`Netlist::add`] rejects nodes that do
/// not exist and parameters that are zero, negative, NaN or infinite, so a
/// successfully built netlist is always structurally valid. [`Netlist::new`]
/// itself cannot fail; a zero node count is only rejected when solving.
#[derive(Clone, Debug, PartialEq)]
pub struct Netlist {
    node_count: usize,
    elements: Vec<Element>,
}

impl Netlist {
    /// Creates an empty netlist with `node_count` nodes numbered
    /// `0..node_count`. Node `0` is ground.
    pub fn new(node_count: usize) -> Self {
        Self {
            node_count,
            elements: Vec::new(),
        }
    }

    /// Number of nodes, including ground.
    pub fn node_count(&self) -> usize {
        self.node_count
    }

    /// Number of elements added so far.
    pub fn len(&self) -> usize {
        self.elements.len()
    }

    /// Whether no elements have been added.
    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    /// The element behind an id, if it exists.
    pub fn element(&self, id: ElementId) -> Option<&Element> {
        self.elements.get(id.0)
    }

    /// All elements, in insertion order.
    pub fn elements(&self) -> &[Element] {
        &self.elements
    }

    /// Adds an element, validating its nodes and parameters.
    ///
    /// On error the netlist is unchanged. On success the returned id is the
    /// element's insertion index.
    pub fn add(&mut self, element: Element) -> Result<ElementId, Error> {
        self.validate_element(&element)?;
        let id = ElementId(self.elements.len());
        self.elements.push(element);
        Ok(id)
    }

    fn validate_element(&self, element: &Element) -> Result<(), Error> {
        if self.node_count == 0 {
            return Err(Error::NoGround);
        }
        element.validate(self.elements.len(), self.node_count)
    }

    /// Validates the whole netlist (node count and every element).
    fn validate(&self) -> Result<(), Error> {
        if self.node_count == 0 {
            return Err(Error::NoGround);
        }
        if self.node_count > MAX_NODES {
            return Err(Error::TooLarge {
                nodes: self.node_count,
            });
        }
        for (index, element) in self.elements.iter().enumerate() {
            element.validate(index, self.node_count)?;
        }
        Ok(())
    }

    /// Solves the DC operating point.
    ///
    /// Capacitors are open, inductors are ideal shorts, and every nonlinear
    /// device is linearised by Newton-Raphson with junction-voltage limiting
    /// and step damping. Returns [`Error::Singular`] for a floating node or a
    /// voltage-source loop, [`Error::NotConverged`] if Newton does not settle.
    pub fn solve_dc(&self) -> Result<Solution, Error> {
        dc::solve_dc(self)
    }

    /// Starts a transient analysis from the DC operating point.
    ///
    /// Capacitor voltages and inductor currents are taken from DC, which is
    /// SPICE's default start. For a circuit that starts discharged (an RC
    /// charging curve from zero), use [`Transient::with_zero_state`] instead.
    pub fn transient(&self) -> Result<Transient, Error> {
        Transient::new(self)
    }

    /// Runs a small-signal AC sweep over `request`.
    ///
    /// The DC operating point is solved first. Nonlinear devices are then
    /// linearised there (diode junction conductance, BJT and MOSFET
    /// transconductances, finite op-amp gain), and the resulting complex
    /// nodal system is solved once per frequency. A DC [`Element::VoltageSource`]
    /// is a zero AC source, so use [`Element::AcVoltageSource`] for the
    /// stimulus. See the crate documentation for the device models used.
    ///
    /// Frequencies are evaluated in exactly the order the request describes,
    /// and the result is deterministic: the same netlist and request yield
    /// bit-identical phasors.
    pub fn ac_sweep(&self, request: AcRequest) -> Result<AcSweep, Error> {
        ac::sweep(self, request)
    }
}

/// The result of one DC or transient solve.
#[derive(Clone, Debug, PartialEq)]
pub struct Solution {
    /// Voltage at each node, in volts. `node_volts[0]` is ground and is always
    /// `0.0`.
    pub node_volts: Vec<f64>,
    /// Branch currents, in amperes, in element insertion order, for the
    /// elements that own a branch in this analysis (voltage sources, inductors
    /// at DC, op-amps). Positive means current flows from the element's first
    /// terminal to its second through the element.
    pub source_currents: Vec<f64>,
    /// Current through every element, indexed like [`Netlist::elements`].
    ///
    /// This is what [`Solution::probe_current`] reads. Capacitors report their
    /// transient current (zero at DC), resistors their `V/R`, and three-terminal
    /// devices their main channel current.
    pub element_currents: Vec<f64>,
}

impl Solution {
    /// Voltage at `node`, if it exists.
    pub fn node_voltage(&self, node: Node) -> Result<f64, Error> {
        self.node_volts
            .get(node)
            .copied()
            .ok_or(Error::InvalidNode {
                node,
                node_count: self.node_volts.len(),
            })
    }

    /// Current through an element, in amperes.
    pub fn probe_current(&self, element: ElementId) -> Result<f64, Error> {
        self.element_currents
            .get(element.0)
            .copied()
            .ok_or(Error::UnknownElement(element))
    }

    fn blank(node_count: usize, element_count: usize) -> Self {
        Self {
            node_volts: vec![0.0; node_count],
            source_currents: Vec::new(),
            element_currents: vec![0.0; element_count],
        }
    }
}
