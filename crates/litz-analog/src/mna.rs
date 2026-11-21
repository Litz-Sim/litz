//! Assembly of the modified-nodal-analysis matrix.
//!
//! One function builds `A·x = b` from a netlist for a given trial solution.
//! Linear elements stamp themselves exactly; nonlinear elements stamp their
//! companion model evaluated at the trial point. The solver calls this once per
//! Newton iteration and solves the resulting dense system.

use crate::device;
use crate::{BjtModel, DiodeModel, Element, MosfetModel, Netlist, Node, OpAmpModel, Solution};

/// Which companion models to use.
pub(crate) enum Mode<'a> {
    /// Capacitors open, inductors short.
    Dc,
    /// Backward-Euler companions; `dt` is the step length, `prev_v` is indexed
    /// by node, `prev_i` by element.
    Transient {
        dt: f64,
        prev_v: &'a [f64],
        prev_i: &'a [Option<f64>],
    },
}

impl Mode<'_> {
    fn is_transient(&self) -> bool {
        matches!(self, Self::Transient { .. })
    }
}

/// Row/column layout of the MNA system for one analysis.
pub(crate) struct System {
    /// Total unknowns: node voltages plus branch currents.
    pub unknowns: usize,
    /// Unknown index of node `k` is `k - 1` (ground is omitted).
    pub node_unknowns: usize,
    /// Branch unknown per element, in insertion order.
    pub branch_of: Vec<Option<usize>>,
}

impl System {
    pub(crate) fn new(netlist: &Netlist, transient: bool) -> Self {
        let node_unknowns = netlist.node_count().saturating_sub(1);
        let mut branch_of = vec![None; netlist.len()];
        let mut branches = 0;
        for (index, element) in netlist.elements().iter().enumerate() {
            let needs = if transient {
                element.has_transient_branch()
            } else {
                element.has_dc_branch()
            };
            if needs {
                branch_of[index] = Some(branches);
                branches += 1;
            }
        }
        Self {
            unknowns: node_unknowns + branches,
            node_unknowns,
            branch_of,
        }
    }

    pub(crate) fn branch_row(&self, branch: usize) -> usize {
        self.node_unknowns + branch
    }
}

fn node_index(node: Node, node_unknowns: usize) -> Option<usize> {
    if node == 0 {
        None
    } else if node <= node_unknowns {
        Some(node - 1)
    } else {
        // Cannot happen for a netlist built through `add`; degrade to ground
        // rather than panicking if a future caller constructs one directly.
        None
    }
}

fn node_voltage(x: &[f64], node: Node) -> f64 {
    if node == 0 {
        0.0
    } else {
        x.get(node - 1).copied().unwrap_or(0.0)
    }
}

/// Reads a node from the transient state vectors, which are indexed by node id
/// (unlike the solution vector, which omits ground).
fn state_voltage(state: &[f64], node: Node) -> f64 {
    state.get(node).copied().unwrap_or(0.0)
}

fn stamp_conductance(a: &mut [f64], n: usize, first: Option<usize>, second: Option<usize>, g: f64) {
    if let Some(i) = first {
        a[i * n + i] += g;
    }
    if let Some(j) = second {
        a[j * n + j] += g;
    }
    if let (Some(i), Some(j)) = (first, second) {
        a[i * n + j] -= g;
        a[j * n + i] -= g;
    }
}

fn add_rhs(b: &mut [f64], row: Option<usize>, value: f64) {
    if let Some(row) = row {
        b[row] += value;
    }
}

fn stamp_branch(
    a: &mut [f64],
    b: &mut [f64],
    n: usize,
    p: Option<usize>,
    q: Option<usize>,
    branch: usize,
    volts: f64,
) {
    if let Some(p) = p {
        a[p * n + branch] += 1.0;
        a[branch * n + p] += 1.0;
    }
    if let Some(q) = q {
        a[q * n + branch] -= 1.0;
        a[branch * n + q] -= 1.0;
    }
    b[branch] += volts;
}

fn set_entry(a: &mut [f64], n: usize, row: usize, col: Option<usize>, value: f64) {
    if let Some(col) = col {
        a[row * n + col] += value;
    }
}

/// Builds the linear system at the trial solution `x`.
///
/// `junction` carries the previous junction voltage for each element, in the
/// element's slot `[vbe, vbc]`; it is updated to the limited values used for
/// this evaluation. Only nonlinear elements read or write it.
pub(crate) fn assemble(
    netlist: &Netlist,
    system: &System,
    mode: &Mode<'_>,
    x: &[f64],
    junction: &mut [[f64; 2]],
) -> (Vec<f64>, Vec<f64>) {
    let n = system.unknowns;
    let mut a = vec![0.0; n * n];
    let mut b = vec![0.0; n];
    let transient = mode.is_transient();

    for (index, element) in netlist.elements().iter().enumerate() {
        match *element {
            Element::Resistor { a: na, b: nb, ohms } => {
                stamp_conductance(
                    &mut a,
                    n,
                    node_index(na, system.node_unknowns),
                    node_index(nb, system.node_unknowns),
                    1.0 / ohms,
                );
            }
            Element::VoltageSource { p, n: q, volts } => {
                if let Some(branch) = system.branch_of[index] {
                    stamp_branch(
                        &mut a,
                        &mut b,
                        n,
                        node_index(p, system.node_unknowns),
                        node_index(q, system.node_unknowns),
                        system.branch_row(branch),
                        volts,
                    );
                }
            }
            // DC and transient: an AC source is a zero-volt source, i.e. a
            // short. Its stimulus belongs to the AC sweep only.
            Element::AcVoltageSource { p, n: q, .. } => {
                if let Some(branch) = system.branch_of[index] {
                    stamp_branch(
                        &mut a,
                        &mut b,
                        n,
                        node_index(p, system.node_unknowns),
                        node_index(q, system.node_unknowns),
                        system.branch_row(branch),
                        0.0,
                    );
                }
            }
            Element::CurrentSource { p, n: q, amps } => {
                add_rhs(&mut b, node_index(p, system.node_unknowns), -amps);
                add_rhs(&mut b, node_index(q, system.node_unknowns), amps);
            }
            Element::Capacitor {
                a: na,
                b: nb,
                farads,
            } => {
                if let Mode::Transient { dt, prev_v, .. } = mode {
                    let g = farads / dt;
                    stamp_conductance(
                        &mut a,
                        n,
                        node_index(na, system.node_unknowns),
                        node_index(nb, system.node_unknowns),
                        g,
                    );
                    let v_prev = state_voltage(prev_v, na) - state_voltage(prev_v, nb);
                    add_rhs(&mut b, node_index(na, system.node_unknowns), g * v_prev);
                    add_rhs(&mut b, node_index(nb, system.node_unknowns), -g * v_prev);
                }
                // DC: a capacitor is an open circuit, and stamps nothing.
            }
            Element::Inductor {
                a: na,
                b: nb,
                henries,
            } => {
                if transient {
                    let Mode::Transient { dt, prev_i, .. } = mode else {
                        continue;
                    };
                    let g = dt / henries;
                    let i_prev = prev_i.get(index).copied().flatten().unwrap_or(0.0);
                    stamp_conductance(
                        &mut a,
                        n,
                        node_index(na, system.node_unknowns),
                        node_index(nb, system.node_unknowns),
                        g,
                    );
                    add_rhs(&mut b, node_index(na, system.node_unknowns), -i_prev);
                    add_rhs(&mut b, node_index(nb, system.node_unknowns), i_prev);
                } else if let Some(branch) = system.branch_of[index] {
                    stamp_branch(
                        &mut a,
                        &mut b,
                        n,
                        node_index(na, system.node_unknowns),
                        node_index(nb, system.node_unknowns),
                        system.branch_row(branch),
                        0.0,
                    );
                }
            }
            Element::Diode {
                anode,
                cathode,
                model,
            } => {
                stamp_diode(
                    &mut a,
                    &mut b,
                    n,
                    system,
                    anode,
                    cathode,
                    &model,
                    x,
                    &mut junction[index],
                );
            }
            Element::Bjt {
                collector,
                base,
                emitter,
                model,
            } => {
                stamp_bjt(
                    &mut a,
                    &mut b,
                    n,
                    system,
                    collector,
                    base,
                    emitter,
                    &model,
                    x,
                    &mut junction[index],
                );
            }
            Element::Mosfet {
                drain,
                gate,
                source,
                model,
            } => {
                stamp_mosfet(&mut a, &mut b, n, system, drain, gate, source, &model, x);
            }
            Element::OpAmp {
                plus,
                minus,
                out,
                model,
            } => {
                stamp_opamp(
                    &mut a, &mut b, n, system, index, plus, minus, out, &model, x,
                );
            }
        }
    }

    (a, b)
}

#[allow(clippy::too_many_arguments)]
fn stamp_diode(
    a: &mut [f64],
    b: &mut [f64],
    n: usize,
    system: &System,
    anode: Node,
    cathode: Node,
    model: &DiodeModel,
    x: &[f64],
    junction: &mut [f64; 2],
) {
    let v_new = node_voltage(x, anode) - node_voltage(x, cathode);
    let evaluated = device::diode(v_new, junction[0], model);
    junction[0] = evaluated.voltage;

    let ia = node_index(anode, system.node_unknowns);
    let ik = node_index(cathode, system.node_unknowns);
    stamp_conductance(a, n, ia, ik, evaluated.conductance);
    // The companion source is the current left after the conductance term.
    let cst = evaluated.current - evaluated.conductance * evaluated.voltage;
    add_rhs(b, ia, -cst);
    add_rhs(b, ik, cst);
}

#[allow(clippy::too_many_arguments)]
fn stamp_bjt(
    a: &mut [f64],
    b: &mut [f64],
    n: usize,
    system: &System,
    collector: Node,
    base: Node,
    emitter: Node,
    model: &BjtModel,
    x: &[f64],
    junction: &mut [f64; 2],
) {
    let vbe = node_voltage(x, base) - node_voltage(x, emitter);
    let vbc = node_voltage(x, base) - node_voltage(x, collector);
    let evaluated = device::bjt(vbe, vbc, junction[0], junction[1], model);
    junction[0] = evaluated.vbe;
    junction[1] = evaluated.vbc;

    let ib = node_index(base, system.node_unknowns);
    let ic = node_index(collector, system.node_unknowns);
    let ie = node_index(emitter, system.node_unknowns);

    let a_be = evaluated.d_ic_dvbe;
    let a_bc = evaluated.d_ic_dvbc;
    let b_be = evaluated.d_ib_dvbe;
    let b_bc = evaluated.d_ib_dvbc;
    let e_be = -(a_be + b_be);
    let e_bc = -(a_bc + b_bc);

    // Collector current, linearised around the limited junction voltages.
    let cst_c = evaluated.ic - a_be * evaluated.vbe - a_bc * evaluated.vbc;
    stamp_three_terminal(a, b, n, ic, ib, ic, ie, a_be, a_bc, cst_c);
    // Base current.
    let cst_b = evaluated.ib - b_be * evaluated.vbe - b_bc * evaluated.vbc;
    stamp_three_terminal(a, b, n, ib, ib, ic, ie, b_be, b_bc, cst_b);
    // Emitter current.
    let cst_e = evaluated.ie - e_be * evaluated.vbe - e_bc * evaluated.vbc;
    stamp_three_terminal(a, b, n, ie, ib, ic, ie, e_be, e_bc, cst_e);
}

/// Stamps one terminal current of a three-terminal device.
///
/// The current into `row` is `m_be·(Vb - Ve) + m_bc·(Vb - Vc) + cst`.
#[allow(clippy::too_many_arguments)]
fn stamp_three_terminal(
    a: &mut [f64],
    b: &mut [f64],
    n: usize,
    row: Option<usize>,
    base: Option<usize>,
    collector: Option<usize>,
    emitter: Option<usize>,
    m_be: f64,
    m_bc: f64,
    cst: f64,
) {
    let Some(row) = row else {
        return;
    };
    if let Some(col) = base {
        a[row * n + col] += m_be + m_bc;
    }
    if let Some(col) = emitter {
        a[row * n + col] -= m_be;
    }
    if let Some(col) = collector {
        a[row * n + col] -= m_bc;
    }
    b[row] -= cst;
}

#[allow(clippy::too_many_arguments)]
fn stamp_mosfet(
    a: &mut [f64],
    b: &mut [f64],
    n: usize,
    system: &System,
    drain: Node,
    gate: Node,
    source: Node,
    model: &MosfetModel,
    x: &[f64],
) {
    let vgs = node_voltage(x, gate) - node_voltage(x, source);
    let vds = node_voltage(x, drain) - node_voltage(x, source);
    let evaluated = device::mosfet(vgs, vds, model);

    let id = node_index(drain, system.node_unknowns);
    let ig = node_index(gate, system.node_unknowns);
    let is = node_index(source, system.node_unknowns);

    let gm = evaluated.gm;
    let gds = evaluated.gds;
    let cst = evaluated.id - gm * evaluated.vgs - gds * evaluated.vds;

    // Drain: Id = gm·(Vg - Vs) + gds·(Vd - Vs) + cst.
    if let Some(row) = id {
        set_entry(a, n, row, ig, gm);
        set_entry(a, n, row, id, gds);
        set_entry(a, n, row, is, -(gm + gds));
        b[row] -= cst;
    }
    // Source: -Id.
    if let Some(row) = is {
        set_entry(a, n, row, ig, -gm);
        set_entry(a, n, row, id, -gds);
        set_entry(a, n, row, is, gm + gds);
        b[row] += cst;
    }
}

#[allow(clippy::too_many_arguments)]
fn stamp_opamp(
    a: &mut [f64],
    b: &mut [f64],
    n: usize,
    system: &System,
    index: usize,
    plus: Node,
    minus: Node,
    out: Node,
    model: &OpAmpModel,
    x: &[f64],
) {
    let Some(branch) = system.branch_of[index] else {
        return;
    };
    let row = system.branch_row(branch);
    let vp = node_voltage(x, plus);
    let vn = node_voltage(x, minus);
    let vout = node_voltage(x, out);
    let target = model.gain * (vp - vn);

    // Region selection. The target decides first; if it is inside the rails
    // but the output node itself is already outside them, the device is
    // saturated regardless (the classic zero-input-difference fixed point of
    // an ideal follower cannot be allowed to sit beyond its supply).
    let region = if target > model.rail_high {
        1
    } else if target < model.rail_low {
        -1
    } else if vout > model.rail_high {
        1
    } else if vout < model.rail_low {
        -1
    } else {
        0
    };

    if region > 0 {
        set_entry(a, n, row, node_index(out, system.node_unknowns), 1.0);
        b[row] += model.rail_high;
    } else if region < 0 {
        set_entry(a, n, row, node_index(out, system.node_unknowns), 1.0);
        b[row] += model.rail_low;
    } else {
        set_entry(a, n, row, node_index(out, system.node_unknowns), 1.0);
        set_entry(
            a,
            n,
            row,
            node_index(plus, system.node_unknowns),
            -model.gain,
        );
        set_entry(
            a,
            n,
            row,
            node_index(minus, system.node_unknowns),
            model.gain,
        );
    }
    // The output branch current leaves the output node. The constraint row
    // already holds its output-node coefficient above; only the symmetric
    // KCL entry is added here.
    if let Some(out) = node_index(out, system.node_unknowns) {
        a[out * n + row] += 1.0;
    }
}

/// Extracts the solution vectors from a solved `x`.
pub(crate) fn solution(netlist: &Netlist, system: &System, x: &[f64], mode: &Mode<'_>) -> Solution {
    let mut solution = Solution::blank(netlist.node_count(), netlist.len());
    for node in 1..netlist.node_count() {
        solution.node_volts[node] = node_voltage(x, node);
    }

    for index in 0..netlist.len() {
        if let Some(branch) = system.branch_of[index] {
            let row = system.branch_row(branch);
            solution
                .source_currents
                .push(x.get(row).copied().unwrap_or(0.0));
        }
    }

    for (index, element) in netlist.elements().iter().enumerate() {
        solution.element_currents[index] = element_current(system, index, element, x, mode);
    }
    solution
}

fn element_current(
    system: &System,
    index: usize,
    element: &Element,
    x: &[f64],
    mode: &Mode<'_>,
) -> f64 {
    let v = |node: Node| node_voltage(x, node);
    let branch = || {
        system
            .branch_of
            .get(index)
            .copied()
            .flatten()
            .map(|b| x.get(system.branch_row(b)).copied().unwrap_or(0.0))
            .unwrap_or(0.0)
    };
    match *element {
        Element::Resistor { a, b, ohms } => (v(a) - v(b)) / ohms,
        Element::VoltageSource { .. } | Element::AcVoltageSource { .. } => branch(),
        Element::CurrentSource { amps, .. } => amps,
        Element::Capacitor { a, b, farads } => match mode {
            Mode::Dc => 0.0,
            Mode::Transient { dt, prev_v, .. } => {
                let v_prev = state_voltage(prev_v, a) - state_voltage(prev_v, b);
                farads / dt * ((v(a) - v(b)) - v_prev)
            }
        },
        Element::Inductor { a, b, henries } => match mode {
            Mode::Dc => branch(),
            Mode::Transient { dt, prev_i, .. } => {
                let i_prev = prev_i.get(index).copied().flatten().unwrap_or(0.0);
                dt / henries * (v(a) - v(b)) + i_prev
            }
        },
        Element::Diode {
            anode,
            cathode,
            model,
        } => {
            let voltage = v(anode) - v(cathode);
            device::diode(voltage, voltage, &model).current
        }
        Element::Bjt {
            collector,
            base,
            emitter,
            model,
        } => {
            let vbe = v(base) - v(emitter);
            let vbc = v(base) - v(collector);
            device::bjt(vbe, vbc, vbe, vbc, &model).ic
        }
        Element::Mosfet {
            drain,
            gate,
            source,
            model,
        } => {
            let vgs = v(gate) - v(source);
            let vds = v(drain) - v(source);
            device::mosfet(vgs, vds, &model).id
        }
        Element::OpAmp { .. } => branch(),
    }
}
