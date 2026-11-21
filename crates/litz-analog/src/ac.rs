//! Small-signal AC analysis: a complex MNA solve at each frequency.
//!
//! The circuit is first solved at DC. Nonlinear devices are then replaced by
//! their small-signal companion models evaluated at that operating point:
//!
//! * a diode by its junction conductance `(Id + Is)/(n·Vt)`,
//! * a BJT by the four current derivatives the DC Newton loop already uses,
//! * a MOSFET by `gm` and `gds`,
//! * an op-amp by its finite gain, or by zero gain when its DC output sits on a
//!   rail (a saturated amplifier does not respond to a small input change).
//!
//! Linear elements keep their physical impedance: a resistor is `1/R`, a
//! capacitor `jωC`, an inductor `1/(jωL)` expressed as a branch constraint
//! `Vp - Vq = jωL·I`. At `ω = 0` the capacitor is open and the inductor a
//! short, which is exactly the DC companion model.
//!
//! There is no frequency dependence inside the device models themselves: no
//! junction capacitance, no transistor charge storage, no op-amp gain-bandwidth
//! product. The result is the linear network that the DC operating point
//! defines, which is the model this crate documents.
//!
//! Determinism: frequencies are visited in the order the request produced, the
//! matrix is assembled in element insertion order, and the complex solver takes
//! fixed pivots. Two runs of the same netlist and request are bit-identical.

use crate::complex::{self, Complex};
use crate::device;
use crate::mna::System;
use crate::{Element, Error, Netlist, Node, Solution};

/// Cap on the number of sweep points, so a typo cannot allocate without bound.
const MAX_AC_POINTS: usize = 100_000;

/// Frequencies to evaluate, in hertz.
///
/// `Decades` spaces points logarithmically between two positive frequencies;
/// `Frequencies` uses an explicit list, which may include `0` (the DC limit).
/// An explicit list is evaluated in the order given.
#[derive(Clone, Debug, PartialEq)]
pub enum AcRequest {
    /// A log-spaced sweep. Both endpoints are included and the spacing is
    /// `1 / points_per_decade` of a decade; the count is rounded to the nearest
    /// whole number of intervals.
    Decades {
        /// Lowest frequency in hertz. Must be positive (a logarithm of zero is
        /// not a frequency).
        start_hz: f64,
        /// Highest frequency in hertz. Must be greater than `start_hz`.
        stop_hz: f64,
        /// Points per decade, at least 1.
        points_per_decade: usize,
    },
    /// An explicit, ordered list of frequencies in hertz. Must be finite and
    /// non-negative.
    Frequencies(Vec<f64>),
}

impl AcRequest {
    /// Convenience constructor for a decade sweep.
    pub fn decades(start_hz: f64, stop_hz: f64, points_per_decade: usize) -> Self {
        Self::Decades {
            start_hz,
            stop_hz,
            points_per_decade,
        }
    }

    /// Convenience constructor for an explicit frequency list.
    pub fn frequencies<I>(frequencies: I) -> Self
    where
        I: IntoIterator<Item = f64>,
    {
        Self::Frequencies(frequencies.into_iter().collect())
    }
}

/// One solved frequency point.
#[derive(Clone, Debug, PartialEq)]
pub struct AcPoint {
    /// Frequency in hertz.
    pub hz: f64,
    /// Complex node voltage phasors in volts, indexed by node. Ground is
    /// `node_volts[0] == 0`.
    pub node_volts: Vec<Complex>,
    /// Branch currents in amperes, in insertion order, for the elements that
    /// own a branch in this analysis (voltage sources, inductors and op-amps).
    /// The sign convention is the DC one: positive from the element's first
    /// terminal to its second through the element.
    pub source_currents: Vec<Complex>,
}

impl AcPoint {
    /// The complex voltage at `node`, in volts.
    pub fn node_voltage(&self, node: Node) -> Result<Complex, Error> {
        self.node_volts
            .get(node)
            .copied()
            .ok_or(Error::InvalidNode {
                node,
                node_count: self.node_volts.len(),
            })
    }

    /// Magnitude at `node` in decibels relative to 1 V: `20·log10|V|`.
    ///
    /// A node at exactly 0 V reports `-∞ dB`, not an error.
    pub fn magnitude_db(&self, node: Node) -> Result<f64, Error> {
        Ok(20.0 * self.node_voltage(node)?.magnitude().log10())
    }

    /// Phase at `node` in degrees, in `(-180, 180]`.
    pub fn phase_deg(&self, node: Node) -> Result<f64, Error> {
        Ok(self.node_voltage(node)?.phase_deg())
    }
}

/// The result of an AC sweep: one point per requested frequency.
#[derive(Clone, Debug, PartialEq)]
pub struct AcSweep {
    /// The frequencies evaluated, in hertz, in order.
    pub frequencies: Vec<f64>,
    /// One solved point per frequency, in the same order.
    pub points: Vec<AcPoint>,
}

impl AcSweep {
    /// The point at `index`, or `None` if the sweep is shorter.
    pub fn point(&self, index: usize) -> Option<&AcPoint> {
        self.points.get(index)
    }

    /// Renders the sweep as CSV with no file I/O.
    ///
    /// The header is `frequency_hz,node_1_db,node_1_phase_deg,...`; ground is
    /// node 0 and has no column. Every numeric field is written with six
    /// decimals, so the output is stable for a given sweep. A trailing newline
    /// ends the last row.
    pub fn to_csv(&self) -> String {
        let node_count = self
            .points
            .first()
            .map_or(1, |point| point.node_volts.len());
        let mut out = String::from("frequency_hz");
        for node in 1..node_count {
            out.push_str(&format!(",node_{node}_db,node_{node}_phase_deg"));
        }
        out.push('\n');
        for point in &self.points {
            out.push_str(&format!("{:.6}", point.hz));
            for node in 1..node_count {
                let v = point.node_volts[node];
                let db = 20.0 * v.magnitude().log10();
                out.push_str(&format!(",{:.6},{:.6}", db, v.phase_deg()));
            }
            out.push('\n');
        }
        out
    }
}

/// Runs the AC sweep for `netlist`.
pub fn sweep(netlist: &Netlist, request: AcRequest) -> Result<AcSweep, Error> {
    netlist.validate()?;
    let frequencies = frequency_list(&request)?;

    // The linearisation point. An `AcVoltageSource` contributes 0 V here, so a
    // circuit driven only by an AC source is linearised about 0 V, which is
    // exactly what a small-signal analysis means.
    let dc = netlist.solve_dc()?;
    // AC uses the DC layout: voltage sources and AC sources own a branch
    // current, and so does an inductor, whose impedance is the only complex
    // branch impedance.
    let system = System::new(netlist, false);

    let mut points = Vec::with_capacity(frequencies.len());
    for hz in frequencies.iter().copied() {
        let omega = std::f64::consts::TAU * hz;
        let (a, b) = assemble(netlist, &system, &dc, omega)?;
        let x = complex::solve(&a, &b)?;
        points.push(point(netlist, &system, &x, hz));
    }
    Ok(AcSweep {
        frequencies,
        points,
    })
}

/// Turns a request into its concrete frequency list.
fn frequency_list(request: &AcRequest) -> Result<Vec<f64>, Error> {
    let invalid = |reason: &'static str| Err(Error::InvalidAcRequest(reason));
    match request {
        AcRequest::Decades {
            start_hz,
            stop_hz,
            points_per_decade,
        } => {
            if !start_hz.is_finite() || *start_hz <= 0.0 {
                return invalid("sweep start_hz must be positive and finite");
            }
            if !stop_hz.is_finite() || *stop_hz <= 0.0 {
                return invalid("sweep stop_hz must be positive and finite");
            }
            if stop_hz <= start_hz {
                return invalid("sweep stop_hz must be greater than start_hz");
            }
            if *points_per_decade == 0 {
                return invalid("points_per_decade must be at least 1");
            }
            let log_start = start_hz.log10();
            let log_stop = stop_hz.log10();
            // Round to the nearest whole interval; a very narrow band still
            // gets its two endpoints rather than an error.
            let intervals = ((log_stop - log_start) * *points_per_decade as f64).round();
            if !intervals.is_finite() {
                return invalid("sweep span is not finite");
            }
            let intervals = intervals.max(1.0) as usize;
            if intervals + 1 > MAX_AC_POINTS {
                return Err(Error::TooManyAcPoints {
                    points: intervals + 1,
                });
            }
            let mut frequencies = Vec::with_capacity(intervals + 1);
            for index in 0..=intervals {
                // Interpolate in log space and pin the endpoints exactly, so
                // the requested start and stop appear unchanged.
                if index == 0 {
                    frequencies.push(*start_hz);
                } else if index == intervals {
                    frequencies.push(*stop_hz);
                } else {
                    let t = index as f64 / intervals as f64;
                    frequencies.push(10f64.powf(log_start + t * (log_stop - log_start)));
                }
            }
            Ok(frequencies)
        }
        AcRequest::Frequencies(frequencies) => {
            if frequencies.is_empty() {
                return invalid("frequency list is empty");
            }
            if frequencies.len() > MAX_AC_POINTS {
                return Err(Error::TooManyAcPoints {
                    points: frequencies.len(),
                });
            }
            for &hz in frequencies {
                if !hz.is_finite() || hz < 0.0 {
                    return invalid("frequencies must be finite and non-negative");
                }
            }
            Ok(frequencies.clone())
        }
    }
}

fn node_index(node: Node, node_unknowns: usize) -> Option<usize> {
    if node == 0 || node > node_unknowns {
        None
    } else {
        Some(node - 1)
    }
}

fn node_voltage(dc: &Solution, node: Node) -> f64 {
    dc.node_volts.get(node).copied().unwrap_or(0.0)
}

fn stamp_admittance(
    a: &mut [Complex],
    n: usize,
    first: Option<usize>,
    second: Option<usize>,
    g: Complex,
) {
    if let Some(i) = first {
        a[i * n + i] = a[i * n + i] + g;
    }
    if let Some(j) = second {
        a[j * n + j] = a[j * n + j] + g;
    }
    if let (Some(i), Some(j)) = (first, second) {
        a[i * n + j] = a[i * n + j] - g;
        a[j * n + i] = a[j * n + i] - g;
    }
}

fn add_rhs(b: &mut [Complex], row: Option<usize>, value: Complex) {
    if let Some(row) = row {
        b[row] = b[row] + value;
    }
}

fn add_entry(a: &mut [Complex], n: usize, row: usize, col: Option<usize>, value: Complex) {
    if let Some(col) = col {
        a[row * n + col] = a[row * n + col] + value;
    }
}

/// Stamps a branch with `V[p] - V[q] = volts + z·I`.
///
/// For a source `z` is zero; for an inductor `z = jωL`, which is what turns the
/// DC short into an impedance at non-zero frequency. The sign of the branch
/// current is the DC convention: positive from `p` to `q` through the element.
#[allow(clippy::too_many_arguments)]
fn stamp_branch(
    a: &mut [Complex],
    b: &mut [Complex],
    n: usize,
    p: Option<usize>,
    q: Option<usize>,
    branch: usize,
    volts: Complex,
    z: Complex,
) {
    if let Some(p) = p {
        a[p * n + branch] = a[p * n + branch] + Complex::ONE;
        a[branch * n + p] = a[branch * n + p] + Complex::ONE;
    }
    if let Some(q) = q {
        a[q * n + branch] = a[q * n + branch] - Complex::ONE;
        a[branch * n + q] = a[branch * n + q] - Complex::ONE;
    }
    a[branch * n + branch] = a[branch * n + branch] - z;
    b[branch] = b[branch] + volts;
}

/// Stamps one terminal current of a linearised three-terminal device.
///
/// The AC perturbation of the terminal current is
/// `m_be·(Vb - Ve) + m_bc·(Vb - Vc)`; the constant operating-point current is
/// deliberately absent, because a small-signal analysis keeps only the
/// incremental part.
#[allow(clippy::too_many_arguments)]
fn stamp_three_terminal(
    a: &mut [Complex],
    n: usize,
    row: Option<usize>,
    base: Option<usize>,
    collector: Option<usize>,
    emitter: Option<usize>,
    m_be: f64,
    m_bc: f64,
) {
    let Some(row) = row else {
        return;
    };
    if let Some(col) = base {
        a[row * n + col] = a[row * n + col] + Complex::new(m_be + m_bc, 0.0);
    }
    if let Some(col) = emitter {
        a[row * n + col] = a[row * n + col] - Complex::new(m_be, 0.0);
    }
    if let Some(col) = collector {
        a[row * n + col] = a[row * n + col] - Complex::new(m_bc, 0.0);
    }
}

/// Assembles the complex system `A·x = b` at angular frequency `omega`.
fn assemble(
    netlist: &Netlist,
    system: &System,
    dc: &Solution,
    omega: f64,
) -> Result<(Vec<Complex>, Vec<Complex>), Error> {
    let n = system.unknowns;
    let mut a = vec![Complex::ZERO; n * n];
    let mut b = vec![Complex::ZERO; n];
    let n_nodes = system.node_unknowns;

    for (index, element) in netlist.elements().iter().enumerate() {
        match *element {
            Element::Resistor { a: na, b: nb, ohms } => {
                stamp_admittance(
                    &mut a,
                    n,
                    node_index(na, n_nodes),
                    node_index(nb, n_nodes),
                    Complex::new(1.0 / ohms, 0.0),
                );
            }
            // A plain DC source is a zero AC source: it fixes an AC ground
            // across its terminals, which is the correct small-signal view of
            // an ideal supply. The AC stimulus is `AcVoltageSource`.
            Element::VoltageSource { p, n: q, .. } => {
                if let Some(branch) = system.branch_of[index] {
                    stamp_branch(
                        &mut a,
                        &mut b,
                        n,
                        node_index(p, n_nodes),
                        node_index(q, n_nodes),
                        system.branch_row(branch),
                        Complex::ZERO,
                        Complex::ZERO,
                    );
                }
            }
            Element::AcVoltageSource {
                p,
                n: q,
                amplitude,
                phase_deg,
            } => {
                if let Some(branch) = system.branch_of[index] {
                    stamp_branch(
                        &mut a,
                        &mut b,
                        n,
                        node_index(p, n_nodes),
                        node_index(q, n_nodes),
                        system.branch_row(branch),
                        Complex::from_polar(amplitude, phase_deg),
                        Complex::ZERO,
                    );
                }
            }
            // A current source drives its `amps` as a zero-phase AC current.
            Element::CurrentSource { p, n: q, amps } => {
                add_rhs(&mut b, node_index(p, n_nodes), Complex::new(-amps, 0.0));
                add_rhs(&mut b, node_index(q, n_nodes), Complex::new(amps, 0.0));
            }
            Element::Capacitor {
                a: na,
                b: nb,
                farads,
            } => {
                // jωC: at ω = 0 this is an open circuit.
                stamp_admittance(
                    &mut a,
                    n,
                    node_index(na, n_nodes),
                    node_index(nb, n_nodes),
                    Complex::new(0.0, omega * farads),
                );
            }
            Element::Inductor {
                a: na,
                b: nb,
                henries,
            } => {
                if let Some(branch) = system.branch_of[index] {
                    stamp_branch(
                        &mut a,
                        &mut b,
                        n,
                        node_index(na, n_nodes),
                        node_index(nb, n_nodes),
                        system.branch_row(branch),
                        Complex::ZERO,
                        Complex::new(0.0, omega * henries),
                    );
                }
            }
            Element::Diode {
                anode,
                cathode,
                model,
            } => {
                // The DC current sets the incremental conductance. Evaluating
                // at the solved junction voltage reproduces exactly the
                // conductance Newton used on its final iteration, including
                // the `Is` term that keeps a reverse-biased diode from having a
                // negative conductance.
                let v = node_voltage(dc, anode) - node_voltage(dc, cathode);
                let g = device::diode(v, v, &model).conductance;
                stamp_admittance(
                    &mut a,
                    n,
                    node_index(anode, n_nodes),
                    node_index(cathode, n_nodes),
                    Complex::new(g, 0.0),
                );
            }
            Element::Bjt {
                collector,
                base,
                emitter,
                model,
            } => {
                let vbe = node_voltage(dc, base) - node_voltage(dc, emitter);
                let vbc = node_voltage(dc, base) - node_voltage(dc, collector);
                let q = device::bjt(vbe, vbc, vbe, vbc, &model);
                let ib = node_index(base, n_nodes);
                let ic = node_index(collector, n_nodes);
                let ie = node_index(emitter, n_nodes);

                let a_be = q.d_ic_dvbe;
                let a_bc = q.d_ic_dvbc;
                let b_be = q.d_ib_dvbe;
                let b_bc = q.d_ib_dvbc;
                let e_be = -(a_be + b_be);
                let e_bc = -(a_bc + b_bc);

                stamp_three_terminal(&mut a, n, ic, ib, ic, ie, a_be, a_bc);
                stamp_three_terminal(&mut a, n, ib, ib, ic, ie, b_be, b_bc);
                stamp_three_terminal(&mut a, n, ie, ib, ic, ie, e_be, e_bc);
            }
            Element::Mosfet {
                drain,
                gate,
                source,
                model,
            } => {
                let vgs = node_voltage(dc, gate) - node_voltage(dc, source);
                let vds = node_voltage(dc, drain) - node_voltage(dc, source);
                let m = device::mosfet(vgs, vds, &model);
                let id = node_index(drain, n_nodes);
                let ig = node_index(gate, n_nodes);
                let is = node_index(source, n_nodes);
                let gm = Complex::new(m.gm, 0.0);
                let gds = Complex::new(m.gds, 0.0);

                // Drain: gm·(Vg - Vs) + gds·(Vd - Vs).
                if let Some(row) = id {
                    add_entry(&mut a, n, row, ig, gm);
                    add_entry(&mut a, n, row, id, gds);
                    add_entry(&mut a, n, row, is, -(gm + gds));
                }
                // Source carries the negative of the drain current.
                if let Some(row) = is {
                    add_entry(&mut a, n, row, ig, -gm);
                    add_entry(&mut a, n, row, id, -gds);
                    add_entry(&mut a, n, row, is, gm + gds);
                }
            }
            Element::OpAmp {
                plus,
                minus,
                out,
                model,
            } => {
                let Some(branch) = system.branch_of[index] else {
                    continue;
                };
                let row = system.branch_row(branch);
                // The small-signal gain is the DC gain in the linear region and
                // zero when the DC output is against a rail: a saturated
                // amplifier does not move, so its output is an AC ground.
                let target = model.gain * (node_voltage(dc, plus) - node_voltage(dc, minus));
                let railed = target > model.rail_high
                    || target < model.rail_low
                    || node_voltage(dc, out) > model.rail_high
                    || node_voltage(dc, out) < model.rail_low;

                add_entry(&mut a, n, row, node_index(out, n_nodes), Complex::ONE);
                if !railed {
                    add_entry(
                        &mut a,
                        n,
                        row,
                        node_index(plus, n_nodes),
                        Complex::new(-model.gain, 0.0),
                    );
                    add_entry(
                        &mut a,
                        n,
                        row,
                        node_index(minus, n_nodes),
                        Complex::new(model.gain, 0.0),
                    );
                }
                // The output branch current leaves the output node; the
                // constraint row already holds the output coefficient.
                if let Some(out) = node_index(out, n_nodes) {
                    a[out * n + row] = a[out * n + row] + Complex::ONE;
                }
            }
        }
    }

    Ok((a, b))
}

/// Extracts one point from the solved `x`.
fn point(netlist: &Netlist, system: &System, x: &[Complex], hz: f64) -> AcPoint {
    let mut node_volts = vec![Complex::ZERO; netlist.node_count()];
    for (node, voltage) in node_volts.iter_mut().enumerate().skip(1) {
        *voltage = x.get(node - 1).copied().unwrap_or(Complex::ZERO);
    }
    let mut source_currents = Vec::new();
    for index in 0..netlist.len() {
        if let Some(branch) = system.branch_of[index] {
            let row = system.branch_row(branch);
            source_currents.push(x.get(row).copied().unwrap_or(Complex::ZERO));
        }
    }
    AcPoint {
        hz,
        node_volts,
        source_currents,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decade_sweep_includes_both_endpoints() {
        let request = AcRequest::decades(10.0, 1_000.0, 3);
        let hz = frequency_list(&request).unwrap();
        // Two decades at three points per decade is six intervals: seven
        // points, ending exactly on the requested stop.
        assert_eq!(hz.len(), 7);
        assert_eq!(hz[0], 10.0);
        assert_eq!(hz[6], 1_000.0);
        // Consecutive points are one third of a decade apart.
        assert!((hz[1] / hz[0] - 10f64.powf(1.0 / 3.0)).abs() < 1e-12);
    }

    #[test]
    fn explicit_frequencies_keep_their_order() {
        let request = AcRequest::frequencies([0.0, 100.0, 50.0]);
        assert_eq!(frequency_list(&request).unwrap(), vec![0.0, 100.0, 50.0]);
    }

    #[test]
    fn invalid_requests_are_rejected() {
        assert!(matches!(
            frequency_list(&AcRequest::decades(0.0, 10.0, 1)),
            Err(Error::InvalidAcRequest(_))
        ));
        assert!(matches!(
            frequency_list(&AcRequest::decades(10.0, 10.0, 1)),
            Err(Error::InvalidAcRequest(_))
        ));
        assert!(matches!(
            frequency_list(&AcRequest::decades(1.0, 10.0, 0)),
            Err(Error::InvalidAcRequest(_))
        ));
        assert!(matches!(
            frequency_list(&AcRequest::frequencies([-1.0])),
            Err(Error::InvalidAcRequest(_))
        ));
        assert!(matches!(
            frequency_list(&AcRequest::frequencies(Vec::new())),
            Err(Error::InvalidAcRequest(_))
        ));
    }

    #[test]
    fn csv_header_and_rows_have_one_column_pair_per_node() {
        let sweep = AcSweep {
            frequencies: vec![1.0, 2.0],
            points: vec![
                AcPoint {
                    hz: 1.0,
                    node_volts: vec![Complex::ZERO, Complex::ONE, Complex::new(0.0, 1.0)],
                    source_currents: vec![],
                },
                AcPoint {
                    hz: 2.0,
                    node_volts: vec![Complex::ZERO, Complex::new(0.0, 1.0), Complex::ONE],
                    source_currents: vec![],
                },
            ],
        };
        let csv = sweep.to_csv();
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(
            lines[0],
            "frequency_hz,node_1_db,node_1_phase_deg,node_2_db,node_2_phase_deg"
        );
        assert_eq!(lines.len(), 3);
        // Node 1 is 1∠0°, node 2 is 1∠90°.
        let first: Vec<&str> = lines[1].split(',').collect();
        assert_eq!(first.len(), 5);
        assert_eq!(first[0], "1.000000");
        assert_eq!(first[1], "0.000000");
        assert_eq!(first[2], "0.000000");
        assert_eq!(first[3], "0.000000");
        assert_eq!(first[4], "90.000000");
    }
}
