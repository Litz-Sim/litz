//! DC operating point by damped Newton-Raphson.

use crate::mna::{self, Mode, System};
use crate::{Error, Netlist, Solution};

/// Newton iteration cap. A circuit that has not settled in this many iterations
/// is reported as [`Error::NotConverged`] rather than looping forever.
pub(crate) const MAX_ITERATIONS: usize = 300;

/// Largest node-voltage change one Newton step may take, in volts. A cold start
/// with a 5 V supply therefore takes five steps to arrive, instead of jumping
/// across an exponential. Junction limiting inside the devices handles the
/// rest of the damping.
pub(crate) const MAX_STEP_VOLTS: f64 = 1.0;

/// Convergence threshold on the damped step, in volts (nodes) and amperes
/// (branch currents). The same absolute number is used for both; a converged
/// circuit has branch currents on the order of microamps or more, so this is
/// far below the resolution a caller needs.
pub(crate) const CONVERGENCE_TOL: f64 = 1e-9;

/// Solves the DC operating point of `netlist`.
pub fn solve_dc(netlist: &Netlist) -> Result<Solution, Error> {
    netlist.validate()?;
    let system = System::new(netlist, false);
    let mut x = vec![0.0; system.unknowns];
    let mut junction = vec![[0.0; 2]; netlist.len()];
    newton(netlist, &system, &Mode::Dc, &mut x, &mut junction)?;
    Ok(mna::solution(netlist, &system, &x, &Mode::Dc))
}

/// Runs the damped Newton loop in place, leaving the solution in `x`.
///
/// Each iteration assembles the linearised circuit at the current `x`, solves
/// it, and applies a damped step. The loop stops when the damped step is below
/// [`CONVERGENCE_TOL`]. A singular matrix is returned immediately; a step that
/// produces a non-finite value is treated as non-convergence, never as a
/// result, so no NaN can escape to a caller.
pub(crate) fn newton(
    netlist: &Netlist,
    system: &System,
    mode: &Mode<'_>,
    x: &mut [f64],
    junction: &mut [[f64; 2]],
) -> Result<(), Error> {
    let node_unknowns = system.node_unknowns;
    let mut last = f64::INFINITY;

    for iteration in 0..MAX_ITERATIONS {
        let (a, b) = mna::assemble(netlist, system, mode, x, junction);
        let next = crate::linalg::solve(&a, &b)?;

        let mut max_node_delta: f64 = 0.0;
        for index in 0..node_unknowns {
            let delta = (next.get(index).copied().unwrap_or(0.0)
                - x.get(index).copied().unwrap_or(0.0))
            .abs();
            if delta > max_node_delta {
                max_node_delta = delta;
            }
        }
        let alpha = if max_node_delta > MAX_STEP_VOLTS {
            MAX_STEP_VOLTS / max_node_delta
        } else {
            1.0
        };

        let mut max_delta: f64 = 0.0;
        for (slot, &target) in x.iter_mut().zip(next.iter()) {
            let delta = alpha * (target - *slot);
            if !delta.is_finite() {
                return Err(Error::NotConverged {
                    iterations: iteration + 1,
                    residual: f64::NAN,
                });
            }
            *slot += delta;
            if delta.abs() > max_delta {
                max_delta = delta.abs();
            }
        }
        last = max_delta;
        if max_delta < CONVERGENCE_TOL {
            return Ok(());
        }
    }

    Err(Error::NotConverged {
        iterations: MAX_ITERATIONS,
        residual: last,
    })
}
